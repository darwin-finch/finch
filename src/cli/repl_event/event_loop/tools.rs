use super::*;

impl EventLoop {
    /// Handle a tool result
    pub(super) async fn handle_tool_result(
        &mut self,
        query_id: Uuid,
        round_token: ToolRoundToken,
        tool_id: String,
        result: Result<String>,
    ) -> Result<()> {
        // Physical outcomes remain part of #163's runner audit even when the
        // provider-visible staged round has already been closed. Correlate
        // each active id at most once before attempting history publication.
        let named_turn_finished =
            if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                if turn.active_tool_ids.remove(&tool_id) {
                    turn.effect_journal
                        .extend(runner_effect_records_from_tool_result(&result));
                    let (output, is_error) = match &result {
                        Ok(output) => (output.clone(), false),
                        Err(error) => (error.to_string(), true),
                    };
                    turn.turn_events
                        .push(crate::server::RunnerTurnEvent::Result {
                            tool_id: tool_id.clone(),
                            output,
                            is_error,
                        });
                }
                turn.cancellation_requested && turn.active_tool_ids.is_empty()
            } else {
                false
            };

        let recorded_result = {
            let mut history = self.conversation.write().await;
            history.record_tool_result(query_id, round_token, &tool_id, &result)
        };
        let progress = match recorded_result {
            Ok(progress) => progress,
            Err(error) => {
                tracing::warn!(
                    "Ignoring rejected tool result for query {} tool {}: {}",
                    query_id,
                    tool_id,
                    error
                );
                if let Some((_name, _input, work_unit, row_idx)) =
                    self.active_tool_uses.write().await.remove(&tool_id)
                {
                    work_unit.fail_row(row_idx, "discarded after closed tool round");
                }
                if named_turn_finished {
                    self.finish_named_brain_turn(query_id, String::new()).await;
                }
                return Ok(());
            }
        };

        // Look up the tool's WorkUnit and row index
        let (tool_name, tool_input, work_unit, row_idx) = {
            let mut map = self.active_tool_uses.write().await;
            map.remove(&tool_id).unwrap_or_else(|| {
                // Fallback: create a standalone WorkUnit for untracked tools
                let fallback = self.output_manager.start_work_unit("Tool");
                let row_idx = fallback.add_row(&tool_id);
                (tool_id.clone(), serde_json::Value::Null, fallback, row_idx)
            })
        };

        // Update the row in the WorkUnit with a semantic summary + optional body
        match &result {
            Ok(content) => {
                let (summary, mut body) = tool_result_to_display(&tool_name, content);
                // A provider-native VM submission is executable source, not an
                // opaque tool argument.  Preserve the exact source in the
                // scrollback row so a user can reconcile every `say` chunk and
                // diagnostic with the program that caused it.
                if tool_name == "submit_program" {
                    let language = tool_input
                        .get("language")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("inferred");
                    if let Some(source) =
                        tool_input.get("source").and_then(serde_json::Value::as_str)
                    {
                        let mut source_body = vec![format!("VM source ({language}):")];
                        source_body.extend(source.lines().map(str::to_owned));
                        source_body.append(&mut body);
                        body = source_body;
                    }
                }
                work_unit.complete_row_with_body(row_idx, summary, body);
            }
            Err(e) => {
                // Truncate very long error messages for the row display
                let err_str = e.to_string();
                let short_err = if err_str.len() > 60 {
                    format!("{}…", err_str.chars().take(57).collect::<String>())
                } else {
                    err_str
                };
                work_unit.fail_row(row_idx, short_err);
            }
        }

        // Record tool execution in the graph
        {
            let input_preview = {
                let s = tool_input.to_string();
                if s.chars().count() > 120 {
                    s.chars().take(120).collect::<String>()
                } else {
                    s
                }
            };
            let (output_preview, is_error) = match &result {
                Ok(c) => {
                    let preview = c.chars().take(200).collect::<String>();
                    (preview, false)
                }
                Err(e) => (e.to_string().chars().take(200).collect(), true),
            };
            self.current_graph
                .lock()
                .await
                .add_node(crate::graph::NodeKind::ToolExecution {
                    name: tool_name.clone(),
                    input_preview,
                    output_preview,
                    is_error,
                });
        }

        // Check if tool execution changed the mode (e.g., EnterPlanMode, PresentPlan)
        // and update status bar accordingly
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // Check if all tools for this query have completed
        let metadata = self.query_states.get_metadata(query_id).await;
        if let Some(meta) = metadata {
            if matches!(meta.state, QueryState::ExecutingTools { .. })
                && progress == ToolRoundProgress::Complete
            {
                // Keep the query-level Tools unit live while the provider
                // consumes these results. A later tool round appends rows
                // to the same unit; a final wire program closes it before
                // opening the distinct program-source unit.
                self.finalize_tool_execution(query_id, round_token).await?;
            }
        }

        Ok(())
    }

    /// Finalize tool execution (all tools complete, re-invoke Claude)
    pub(super) async fn finalize_tool_execution(
        &mut self,
        query_id: Uuid,
        round_token: ToolRoundToken,
    ) -> Result<()> {
        let results = match self
            .conversation
            .read()
            .await
            .completed_tool_results(query_id, round_token)
        {
            Ok(results) => results,
            Err(error) => {
                tracing::warn!(
                    "Tool round {} was not ready to finalize: {}",
                    query_id,
                    error
                );
                return Ok(());
            }
        };

        // Sync the plan mode status bar.  handle_present_plan() updates the mode Arc
        // but is a free function without &self access, so the indicator update happens here.
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // ── Plan-approval fast path ───────────────────────────────────────────
        // When the user just approved a PresentPlan, the mode is now Executing.
        // The long planning exploration history confuses the model (it forgets the
        // task and re-explores instead of implementing). Reset to a clean context
        // with just the execution directive.
        if matches!(current_mode, ReplMode::Executing { .. }) {
            let plan_directive = results.iter().find_map(|result| {
                if !result.is_error && result.content.starts_with("Plan approved by user.") {
                    Some(result.content.clone())
                } else {
                    None
                }
            });

            if let Some(directive) = plan_directive {
                // Clear tool-call history so planning-phase reads/globs don't
                // trigger loop detection when Claude calls them again during execution.
                self.tool_call_history.write().await.remove(&query_id);

                // Reset conversation to a single clear execution prompt.
                let mut proposed_history = self.conversation.read().await.clone();
                proposed_history.clear();
                proposed_history.add_user_message(directive);
                if let Err(error) = self.checkpoint_history(&proposed_history) {
                    let _ = self.event_tx.send(ReplEvent::QueryFailed {
                        query_id,
                        error: format!(
                            "Plan continuation was not sent because its checkpoint failed: {error:#}"
                        ),
                    });
                    return Ok(());
                }
                *self.conversation.write().await = proposed_history;

                let _ = self.llm_tx.send(LlmRequest::Query {
                    id: query_id,
                    text: String::new(),
                    no_tools: false,
                    admission: None,
                    admission_ready: None,
                    spawned: None,
                    publication: None,
                });
                return Ok(());
            }
        }

        let checkpoint_path = self.conversation_checkpoint_path();
        let committed = commit_tool_round_and_continue(
            &self.conversation,
            query_id,
            round_token,
            &self.llm_tx,
            checkpoint_path.as_deref(),
        )
        .await;
        if let Err(error) = committed {
            let _ = self.event_tx.send(ReplEvent::QueryFailed {
                query_id,
                error: format!("Tool continuation could not be admitted: {error}"),
            });
            return Ok(());
        }

        Ok(())
    }

    /// Handle tool approval request (show dialog, get user response)
    pub(super) async fn handle_tool_approval_request(
        &mut self,
        query_id: Uuid,
        tool_use: crate::tools::types::ToolUse,
        response_tx: tokio::sync::oneshot::Sender<
            crate::cli::repl_event::events::ConfirmationResult,
        >,
    ) -> Result<()> {
        use crate::cli::tui::Dialog;

        tracing::debug!("[EVENT_LOOP] Requesting tool approval: {}", tool_use.name);

        let approval_audience = self
            .pending_named_brain_turns
            .get(&query_id)
            .map(|turn| turn.approval_audience.clone());
        let approval_tx = self
            .pending_named_brain_turns
            .get(&query_id)
            .and_then(|turn| turn.approval_tx.clone());
        if let (Some(approval_tx), Some(audience)) = (approval_tx, approval_audience.as_ref()) {
            let event = crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: tool_use.id.clone(),
                approval_kind: "tool".to_string(),
                subject: tool_use.name.clone(),
                audience: audience.clone(),
                detail: serde_json::json!({"input": tool_use.input.clone()}),
            };
            let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
            if approval_tx
                .send(crate::server::RunnerApprovalRequest {
                    event,
                    response_tx: decision_tx,
                })
                .is_err()
            {
                let _ = response_tx.send(crate::cli::repl_event::events::ConfirmationResult::Deny);
                return Ok(());
            }
            tokio::spawn(async move {
                let confirmation = decision_rx
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .and_then(|decision| confirmation_from_audit_value(&decision, &tool_use).ok())
                    .unwrap_or(crate::cli::repl_event::events::ConfirmationResult::Deny);
                let _ = response_tx.send(confirmation);
            });
            return Ok(());
        }
        if let (Some(turn), Some(audience)) = (
            self.pending_named_brain_turns.get_mut(&query_id),
            approval_audience.as_ref(),
        ) {
            turn.turn_events
                .push(crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: tool_use.id.clone(),
                    approval_kind: "tool".to_string(),
                    subject: tool_use.name.clone(),
                    audience: audience.clone(),
                    detail: serde_json::json!({"input": tool_use.input.clone()}),
                });
        }

        // Create approval dialog — compact 3-option style matching Claude Code UX
        let tool_name = &tool_use.name;
        let mut summary = tool_approval_summary(&tool_use);
        if let Some(audience) = approval_audience {
            summary.push_str("\n\n");
            summary.push_str(&approval_audience_summary(&audience));
        }

        let dialog = Dialog::tool_approval(tool_name, &summary);

        // Set dialog in TUI (non-blocking - will be handled by async_input task)
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);

        // Force render to show dialog immediately
        if let Err(e) = tui.render() {
            tracing::error!("[EVENT_LOOP] Failed to render dialog: {}", e);
        }
        drop(tui);

        // Store the response channel and tool_use for when dialog completes
        // We'll check pending_dialog_result in the event loop and send the response then
        self.pending_approvals
            .write()
            .await
            .insert(query_id, (tool_use, response_tx));

        tracing::debug!("[EVENT_LOOP] Tool approval dialog shown, waiting for user response");

        Ok(())
    }

    /// Present one exact typed-VM capability request. Unlike legacy tool
    /// approval, these choices become structured grant scopes and are checked
    /// again against the retained ProgramRun before any authority is issued.
    pub(super) async fn handle_vm_approval_request(
        &mut self,
        prompt: crate::vm::ApprovalPrompt,
        response_tx: tokio::sync::oneshot::Sender<crate::vm::ApprovalChoice>,
    ) -> Result<()> {
        if self.pending_vm_approval.is_some() {
            let _ = response_tx.send(crate::vm::ApprovalChoice::Deny);
            self.output_manager.write_error(
                "Denied a concurrent VM capability request while another approval dialog was active",
            );
            return Ok(());
        }

        let query_id = *self.active_query_id.read().await;
        let approval_id = prompt.request.id.to_string();
        let approval_tx = query_id.and_then(|query_id| {
            self.pending_named_brain_turns
                .get(&query_id)
                .and_then(|turn| turn.approval_tx.clone())
        });
        if let (Some(query_id), Some(approval_tx)) = (query_id, approval_tx) {
            let audience = self
                .pending_named_brain_turns
                .get(&query_id)
                .expect("named Brain turn disappeared while requesting approval")
                .approval_audience
                .clone();
            let event = crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: approval_id.clone(),
                approval_kind: "vm_capability".to_string(),
                subject: format!("{:?}", prompt.exact.capability),
                audience,
                detail: serde_json::to_value(&prompt).unwrap_or_else(
                    |_| serde_json::json!({"reason": prompt.request.reason.clone()}),
                ),
            };
            let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
            if approval_tx
                .send(crate::server::RunnerApprovalRequest {
                    event,
                    response_tx: decision_tx,
                })
                .is_err()
            {
                let _ = response_tx.send(crate::vm::ApprovalChoice::Deny);
                return Ok(());
            }
            tokio::spawn(async move {
                let choice = decision_rx
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .and_then(|decision| serde_json::from_value(decision).ok())
                    .unwrap_or(crate::vm::ApprovalChoice::Deny);
                let _ = response_tx.send(choice);
            });
            return Ok(());
        }
        if let Some(query_id) = query_id {
            if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                turn.turn_events
                    .push(crate::server::RunnerTurnEvent::ApprovalRequested {
                        approval_id: approval_id.clone(),
                        approval_kind: "vm_capability".to_string(),
                        subject: format!("{:?}", prompt.exact.capability),
                        audience: turn.approval_audience.clone(),
                        detail: serde_json::to_value(&prompt).unwrap_or_else(
                            |_| serde_json::json!({"reason": prompt.request.reason.clone()}),
                        ),
                    });
            }
        }

        let choices = vm_approval_choices(&prompt);
        let audience = query_id
            .and_then(|query_id| self.pending_named_brain_turns.get(&query_id))
            .map(|turn| &turn.approval_audience);
        let dialog = vm_approval_dialog(&prompt, audience, self.program_runtime.as_ref());

        self.pending_vm_approval = Some(PendingVmApproval {
            response_tx,
            choices,
            query_id,
            approval_id,
        });
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);
        tui.pending_dialog_result = None;
        tui.render()?;
        Ok(())
    }
}
