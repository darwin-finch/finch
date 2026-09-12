use super::*;

impl EventLoop {
    /// Handle an event from the event channel
    pub(super) async fn handle_event(&mut self, event: ReplEvent) -> Result<()> {
        match event {
            ReplEvent::UserInput { input } => {
                self.handle_user_input(input).await?;
            }

            ReplEvent::QueryComplete { query_id, response } => {
                if !self
                    .query_states
                    .try_publish_completion(
                        query_id,
                        response.clone(),
                        response.clone(),
                        &self.conversation,
                    )
                    .await
                {
                    return Ok(());
                }
                // Update compaction percentage in status bar
                self.update_compaction_status().await;

                // Display response
                self.output_manager.write_response(&response);
                if let Err(error) = self.checkpoint_conversation().await {
                    self.report_checkpoint_error(
                        "Response is retained in memory, but its recovery checkpoint failed",
                        &error,
                    );
                }
            }

            ReplEvent::QueryFailed { query_id, error } => {
                // A terminal provider failure closes publication immediately;
                // detached #163 effects may still finish their durable audit.
                self.conversation.write().await.abort_staged(query_id);
                if let Some(turn) = self.pending_named_brain_turns.get(&query_id) {
                    if turn.cancellation_requested {
                        // A cancelled provider may report its terminal error
                        // before independently-running tools quiesce. Retain
                        // the correlation record until every physical outcome
                        // reaches the audit boundary, and never publish this
                        // late provider error into Brain history.
                        if turn.active_tool_ids.is_empty() {
                            self.finish_named_brain_turn(query_id, String::new()).await;
                            if *self.active_query_id.read().await == Some(query_id) {
                                *self.active_query_id.write().await = None;
                            }
                        }
                        return Ok(());
                    }
                }
                // DON'T remove streaming message here - fallback providers need it!
                // The message will be removed on StreamingComplete or stays for final error display

                // Update query state
                self.query_states
                    .update_state(
                        query_id,
                        QueryState::Failed {
                            error: error.clone(),
                        },
                    )
                    .await;

                let transient_output_unit =
                    self.query_states.brain_output_work_unit(query_id).await;
                self.query_states
                    .set_brain_output_work_unit(query_id, None)
                    .await;
                if let Some(unit) = self.query_states.tool_work_unit(query_id).await {
                    unit.set_failed();
                    self.query_states.set_tool_work_unit(query_id, None).await;
                }

                // Display error
                self.output_manager
                    .write_error(format!("Query failed: {}", error));

                if let Some(pending) = self.pending_named_brain_turns.remove(&query_id) {
                    self.agent_scheduler.set_active_brain_parent(None).await;
                    self.local_brain_projections
                        .push_back(failed_local_brain_projection(
                            pending.run_id,
                            &pending.turn_events,
                            transient_output_unit,
                        ));
                    let _ = pending
                        .response_tx
                        .send(Err(crate::server::RunnerTurnError {
                            message: error.clone(),
                            turn_events: pending.turn_events,
                            effect_journal: pending.effect_journal,
                        }));
                }

                // Render TUI to ensure viewport is redrawn after error message
                if let Err(e) = self.render_tui().await {
                    tracing::warn!("Failed to render TUI after query error: {}", e);
                }

                // A terminal provider failure has no later StreamingComplete.
                // Release the turn so queued user input cannot wedge behind it.
                if *self.active_query_id.read().await == Some(query_id) {
                    *self.active_query_id.write().await = None;
                    if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                        self.execute_query_inner(next, echo, chat_only).await?;
                    }
                }
            }

            ReplEvent::ToolResult {
                query_id,
                round_token,
                tool_id,
                mut result,
            } => {
                let named_brain_disposition = self
                    .pending_named_brain_turns
                    .get_mut(&query_id)
                    .map(|turn| turn.observe_tool_result(&tool_id, &result));
                if let Some(NamedBrainToolResultDisposition::DiscardCancelled { quiesced }) =
                    named_brain_disposition
                {
                    // The physical effect has its own durable audit outcome,
                    // but a cancelled Brain turn must never publish a late
                    // ToolResult or feed it into another provider round.
                    if let Some((_name, _input, work_unit, row_idx)) =
                        self.active_tool_uses.write().await.remove(&tool_id)
                    {
                        work_unit.fail_row(row_idx, "discarded after Brain cancellation");
                    }
                    if quiesced {
                        self.finish_named_brain_turn(query_id, String::new()).await;
                        if *self.active_query_id.read().await == Some(query_id) {
                            *self.active_query_id.write().await = None;
                        }
                    }
                    return Ok(());
                }
                if let Some(restart) =
                    crate::tools::implementations::restart::deferred_frontend_restart_from_tool_result(
                        &result,
                    )
                {
                    match self.pending_named_brain_turns.get_mut(&query_id) {
                        Some(turn) if turn.restart.is_none() => {
                            turn.restart = Some(restart);
                        }
                        Some(_) => {
                            result = Err(anyhow::anyhow!(
                                "this Brain turn already has a pending frontend restart"
                            ));
                        }
                        None => {
                            result = Err(anyhow::anyhow!(
                                "frontend restart requires a canonical named-Brain turn"
                            ));
                        }
                    }
                }
                if let Some(proposal) = deferred_proposal_from_tool_result(&result) {
                    self.output_manager.write_status(format!(
                        "Proposal {} is awaiting editor review",
                        proposal.handle.sequence
                    ));
                    let approval_audience = self
                        .pending_named_brain_turns
                        .get(&query_id)
                        .map(|turn| turn.approval_audience.clone());
                    self.spawn_deferred_proposal(
                        query_id,
                        round_token,
                        tool_id,
                        proposal,
                        approval_audience,
                    );
                } else if let Some(approval) = deferred_vm_approval_from_tool_result(&result) {
                    self.output_manager.write_status(format!(
                        "VM capability request {} is awaiting approval",
                        approval.prompt.request.id
                    ));
                    self.spawn_deferred_vm_approval(query_id, round_token, tool_id, approval);
                } else {
                    self.handle_tool_result(query_id, round_token, tool_id, result)
                        .await?;
                }
            }

            ReplEvent::ToolCallsStarted {
                query_id,
                tool_uses,
            } => {
                if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                    turn.observe_tool_calls(tool_uses);
                }
            }

            ReplEvent::ToolApprovalNeeded {
                query_id,
                tool_use,
                response_tx,
            } => {
                self.handle_tool_approval_request(query_id, tool_use, response_tx)
                    .await?;
            }

            ReplEvent::VmApprovalNeeded {
                prompt,
                response_tx,
            } => {
                self.handle_vm_approval_request(prompt, response_tx).await?;
            }

            ReplEvent::OutputReady { message } => {
                self.output_manager.write_status(message);
            }

            ReplEvent::VmEffect {
                projection,
                envelope,
            } => {
                // A reconnecting application may deliver a journal suffix
                // more than once. Let the client-local projection reject
                // duplicates/gaps before rendering notices or mutating a
                // WorkUnit; the durable acknowledgement belongs to the later
                // Brain event-log layer.
                let projected = projection.project_envelope(envelope);
                if projected.is_empty() {
                    return Ok(());
                }
                for envelope in projected {
                    if envelope.effect.requirement.capability
                        != crate::vm::CapabilityKind::ProgramInvoke
                    {
                        continue;
                    }
                    let intent = match &envelope.effect.event {
                        crate::vm::HostSideEffect::Request { arguments } => arguments
                            .get(1)
                            .and_then(|value| match value {
                                crate::vm::TypedValue::String(text) => Some(text.as_str()),
                                _ => None,
                            })
                            .unwrap_or("Review proposed program"),
                        _ => "Review proposed program",
                    };
                    projection.append_default(&format!(
                        "Proposal awaiting review: {intent} [run {}, effect {}]",
                        envelope.execution_id, envelope.effect.sequence
                    ));
                }
                self.render_tui().await?;
            }

            ReplEvent::VmOutputComplete { output_unit } => {
                output_unit.set_complete();
                self.render_tui().await?;
            }

            ReplEvent::VmEffectJournalComplete { query_id, records } => {
                if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                    turn.effect_journal.extend(records);
                }
            }

            ReplEvent::TypedProgramComplete {
                output_unit,
                result,
            } => {
                match result {
                    Ok(outcome)
                        if outcome.status
                            == crate::runtime::outcome::ExecutionStatus::Completed => {}
                    Ok(outcome) => {
                        let detail =
                            outcome.diagnostics.first().cloned().unwrap_or_else(|| {
                                format!("VM program ended as {:?}", outcome.status)
                            });
                        output_unit.append_response(&format!("VM error: {detail}"));
                    }
                    Err(error) => output_unit.append_response(&format!("VM error: {error}")),
                }
                output_unit.set_complete();
                self.render_tui().await?;
            }

            ReplEvent::StreamingComplete {
                query_id,
                full_response,
            } => {
                tracing::debug!("[EVENT_LOOP] Handling StreamingComplete event");

                if let Some(turn) = self.pending_named_brain_turns.get(&query_id) {
                    if turn.cancellation_requested {
                        // Cancellation terminalizes only after the provider
                        // and every launched tool have reached a quiescent
                        // boundary. The provider's late prose is deliberately
                        // excluded from conversation and Brain projections.
                        if turn.active_tool_ids.is_empty() {
                            self.finish_named_brain_turn(query_id, String::new()).await;
                            if *self.active_query_id.read().await == Some(query_id) {
                                *self.active_query_id.write().await = None;
                            }
                        }
                        return Ok(());
                    }
                }

                // Check if this query is executing tools
                // If so, the assistant message was already added with ToolUse blocks
                let state = self
                    .query_states
                    .get_metadata(query_id)
                    .await
                    .map(|m| m.state.clone());
                let is_executing_tools = matches!(state, Some(QueryState::ExecutingTools { .. }));
                // The streaming path adds the assistant message and sets Completed before
                // sending StreamingComplete. The non-streaming path does not — it relies on
                // this handler to do both. Detect which case we are in.
                let already_completed = matches!(state, Some(QueryState::Completed { .. }));
                if matches!(
                    state,
                    Some(QueryState::Cancelled | QueryState::Failed { .. })
                ) {
                    tracing::debug!(
                        "Discarding terminal provider response for closed query {}",
                        query_id
                    );
                    return Ok(());
                }

                if !is_executing_tools && !already_completed {
                    tracing::debug!(
                        "[EVENT_LOOP] No tools, adding assistant message to conversation"
                    );
                    if !self
                        .query_states
                        .try_publish_completion(
                            query_id,
                            full_response.clone(),
                            full_response.clone(),
                            &self.conversation,
                        )
                        .await
                    {
                        return Ok(());
                    }
                    tracing::debug!("[EVENT_LOOP] Published assistant completion");
                } else {
                    tracing::debug!("[EVENT_LOOP] Skipping duplicate message (tools={is_executing_tools}, already_completed={already_completed})");
                }

                // Update context usage indicator now that the message is committed
                self.update_compaction_status().await;
                if let Err(error) = self.checkpoint_conversation().await {
                    self.report_checkpoint_error(
                        "Response is retained in memory, but its recovery checkpoint failed",
                        &error,
                    );
                }

                self.finish_named_brain_turn(query_id, full_response.clone())
                    .await;

                // Render TUI to write the complete message to scrollback
                self.render_tui().await?;
                tracing::debug!("[EVENT_LOOP] StreamingComplete handled, TUI rendered");

                // Clear active query (query completed successfully)
                {
                    let mut active = self.active_query_id.write().await;
                    if *active == Some(query_id) {
                        *active = None;
                    }
                }
                if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                    self.execute_query_inner(next, echo, chat_only).await?;
                }
                // Record final response + save execution graph
                if !is_executing_tools {
                    let preview = full_response.chars().take(300).collect::<String>();
                    let mut g = self.current_graph.lock().await;
                    g.add_node(crate::graph::NodeKind::FinalResponse { preview });
                    if let Err(e) = g.save() {
                        tracing::warn!("Failed to save execution graph: {}", e);
                    }
                }

                // The AI does NOT auto-push to the stack on completion.
                // It pushes explicitly via the Push tool when it wants to
                // add something to the collaborative program.

                // Clear per-query tool-call history so it doesn't grow forever.
                self.tool_call_history.write().await.remove(&query_id);
            }

            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
                primary_allowance_used_percent,
                secondary_allowance_used_percent,
            } => {
                tracing::debug!(
                    primary_allowance_used_percent,
                    secondary_allowance_used_percent,
                    "Provider subscription allowance snapshot"
                );
                // Record LLM invocation in execution graph
                self.current_graph
                    .lock()
                    .await
                    .add_node(crate::graph::NodeKind::LlmCall {
                        model: model.clone(),
                        input_tokens,
                        output_tokens,
                    });
                // Update status bar with live stats
                self.status_bar
                    .update_live_stats(model, input_tokens, output_tokens, latency_ms);
                // Render to display updated stats
                self.render_tui().await?;
            }

            ReplEvent::AgentLifecycle(event) => {
                let finished = match &event {
                    crate::runtime::scheduler::AgentEvent::TaskFinished { result } => {
                        Some(result.clone())
                    }
                    _ => None,
                };
                self.tui_renderer.lock().await.apply_activity(
                    crate::cli::repl_event::activity_view::agent_activity(&event),
                );
                if let Some(result) = finished {
                    let summary = if result.final_message.trim().is_empty() {
                        result.diagnostics.join("; ")
                    } else {
                        result.final_message
                    };
                    self.output_manager.write_info(format!(
                        "child {} {:?} ({} turns, {} ms)\n{}",
                        result.identity.agent_id,
                        result.status,
                        result.turns,
                        result.elapsed_ms,
                        summary
                    ));
                }
                self.render_tui().await?;
            }

            ReplEvent::CancelQuery => {
                // Get the active query ID
                let query_id = {
                    let active = self.active_query_id.read().await;
                    *active
                };

                if let Some(qid) = query_id {
                    // Fire the per-query cancellation token so handle_present_plan
                    // (and any other token-aware loops) can detect the cancel immediately.
                    if !self.query_states.cancel_query(qid).await {
                        tracing::debug!("Ignoring cancellation for already-terminal query {}", qid);
                        return Ok(());
                    }
                    self.conversation.write().await.abort_staged(qid);
                    self.close_active_tool_rows(qid, "cancelled").await;
                    let named_turn =
                        if let Some(pending) = self.pending_named_brain_turns.get_mut(&qid) {
                            pending.cancellation_requested = true;
                            true
                        } else {
                            false
                        };

                    if !named_turn {
                        *self.active_query_id.write().await = None;
                        self.tool_call_history.write().await.remove(&qid);
                    }

                    // If we were in plan/executing mode, cancel that too so the
                    // user doesn't have to press Ctrl+C again to escape.
                    {
                        let mode = self.mode.read().await.clone();
                        if !matches!(mode, ReplMode::Normal) {
                            *self.mode.write().await = ReplMode::Normal;
                            self.update_plan_mode_indicator(&ReplMode::Normal);
                        }
                    }

                    // Show cancellation message
                    self.output_manager.write_info(if named_turn {
                        "⚠️  Cancellation requested; waiting for the named Brain turn to reach a safe boundary"
                    } else {
                        "⚠️  Query cancelled by user (Ctrl+C)"
                    });
                    self.render_tui().await?;

                    tracing::info!("Query {} cancellation requested by user", qid);
                } else {
                    // No active query — Ctrl+C when idle:
                    //   • in plan/executing mode → exit that mode, stay in finch
                    //   • in normal mode → exit finch entirely (like /quit)
                    let mode = self.mode.read().await.clone();
                    if !matches!(mode, ReplMode::Normal) {
                        *self.mode.write().await = ReplMode::Normal;
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan mode cancelled (Ctrl+C).");
                        self.render_tui().await?;
                    } else {
                        let _ = self.event_tx.send(ReplEvent::Shutdown);
                    }
                }
            }

            ReplEvent::Shutdown => {
                // Handled in run() method - this should not be reached
                unreachable!("Shutdown event should be handled in run() method");
            }

            ReplEvent::PosetComplete { result } => {
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {
                        self.output_manager.write_info("📚 Program complete.");
                    }
                    Err(e) => {
                        self.output_manager.write_info(format!("📚 Error: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::LispResult { result } => {
                match result {
                    Ok(text) if text != "()" && !text.is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.output_manager.write_info(format!("lisp: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::RemoteBrainMessage { target, message } => {
                let is_current = self.selected_brain_matches(&target);
                if is_current {
                    let acknowledged_seq = match &message {
                        crate::brain::store::BrainWireMessage::Snapshot { brain } => brain.revision,
                        crate::brain::store::BrainWireMessage::Event { event } => event.seq,
                    };
                    self.render_remote_brain_message(message).await?;
                    if let Some(client) = self.selected_brain_mut() {
                        if let Err(error) = client.acknowledge(acknowledged_seq).await {
                            self.output_manager
                                .write_info(format!("{target}: could not save cursor: {error}"));
                            self.render_tui().await?;
                        }
                    }
                }
            }
            ReplEvent::RemoteBrainError { target, error } => {
                self.output_manager.write_info(format!("{target}: {error}"));
                self.render_tui().await?;
            }
            ReplEvent::RemoteBrainDisconnected { target } => {
                let is_current = self.selected_brain_matches(&target);
                if is_current {
                    self.clear_remote_brain_approvals_for_target(&target).await;
                    let role = if self.selected_brain_is_home() {
                        "home"
                    } else {
                        "driver"
                    };
                    self.status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::SessionLabel,
                        format!("◆ brain: {target} · {role} · disconnected"),
                    );
                    self.output_manager.write_info(format!(
                        "{target}: Brain event connection closed; detach or reattach to reconnect"
                    ));
                    self.render_tui().await?;
                }
            }
            ReplEvent::HomeBrainMessage { epoch, message } => {
                if epoch != self.home_watch_epoch {
                    return Ok(());
                }
                let acknowledged_seq = match &message {
                    crate::brain::store::BrainWireMessage::Snapshot { brain } => brain.revision,
                    crate::brain::store::BrainWireMessage::Event { event } => event.seq,
                };
                if self.active_remote_brain.is_none() {
                    self.render_remote_brain_message(message).await?;
                }
                if let Some(client) = self.home_brain.as_mut() {
                    if let Err(error) = client.acknowledge(acknowledged_seq).await {
                        let detail = format!("home cursor acknowledgement failed: {error}");
                        if self.last_home_watch_error.as_deref() != Some(&detail) {
                            self.output_manager.write_info(detail.clone());
                        }
                        self.last_home_watch_error = Some(detail);
                    }
                }
            }
            ReplEvent::HomeBrainWatchFailed { epoch, error } => {
                if epoch != self.home_watch_epoch {
                    return Ok(());
                }
                self.home_brain = None;
                let detail = error.unwrap_or_else(|| "connection closed".into());
                if self.last_home_watch_error.as_deref() != Some(&detail) {
                    self.output_manager.write_info(format!(
                        "{}: home event watch unavailable: {}; reconnecting (runner callback is {})",
                        self.session_label,
                        detail,
                        if self.home_runner_lease_active { "still registered" } else { "offline" },
                    ));
                }
                self.last_home_watch_error = Some(detail);
                if self.active_remote_brain.is_none() {
                    self.status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::SessionLabel,
                        format!(
                            "◆ {} · {} · event watch reconnecting",
                            self.session_label,
                            if self.home_runner_lease_active {
                                "runner"
                            } else {
                                "runner offline"
                            },
                        ),
                    );
                    self.render_tui().await?;
                }
                self.schedule_home_brain_reconnect(epoch, 0);
            }
            ReplEvent::ReconnectHomeBrain { epoch, attempt } => {
                if epoch != self.home_watch_epoch || self.home_brain.is_some() {
                    return Ok(());
                }
                match self.reconnect_home_brain().await {
                    Ok(()) => {
                        self.output_manager.write_info(format!(
                            "{}: home event watch reconnected; runner callback {}",
                            self.session_label,
                            if self.home_runner_lease_active {
                                "registered"
                            } else {
                                "still retrying"
                            },
                        ));
                        self.render_tui().await?;
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        if self.last_home_watch_error.as_deref() != Some(&detail) {
                            self.output_manager.write_info(format!(
                                "{}: home reconnect attempt failed: {}",
                                self.session_label, detail
                            ));
                        }
                        self.last_home_watch_error = Some(detail);
                        self.schedule_home_brain_reconnect(
                            self.home_watch_epoch,
                            attempt.saturating_add(1),
                        );
                    }
                }
            }
            ReplEvent::ReconnectHomeRunner {
                epoch,
                attempt,
                target,
            } => {
                if epoch
                    != self
                        .runner_renewal_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                    || self.home_runner_lease_active
                {
                    return Ok(());
                }
                match self.restore_home_runner(target.clone()).await {
                    Ok(()) => {
                        self.output_manager.write_info(format!(
                            "{}: runner callback reconnected",
                            self.session_label
                        ));
                        if self.active_remote_brain.is_none() {
                            self.update_remote_brain_status(true);
                            self.render_tui().await?;
                        }
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        if self.last_home_runner_error.as_deref() != Some(&detail) {
                            self.output_manager.write_info(format!(
                                "{}: runner reconnect attempt failed: {}",
                                self.session_label, detail
                            ));
                        }
                        self.last_home_runner_error = Some(detail);
                        if self
                            .last_home_runner_error
                            .as_deref()
                            .is_some_and(|detail| detail.contains("handed off"))
                        {
                            self.home_runner_lease_id = None;
                            self.runner_reconnect_target = None;
                        } else {
                            self.schedule_home_runner_reconnect(
                                epoch,
                                attempt.saturating_add(1),
                                target,
                            );
                        }
                    }
                }
            }
            ReplEvent::RunnerLeaseStatus {
                brain,
                environment,
                epoch,
                lease_id,
                detail,
            } => {
                if epoch
                    != self
                        .runner_renewal_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Ok(());
                }
                let registration = match (lease_id, self.ipc_client.as_ref()) {
                    (Some(lease_id), Some(ipc)) => match ipc
                        .register_brain_runner(&brain, lease_id, self.event_tx.clone())
                        .await
                    {
                        Ok(bootstrap) => {
                            match self
                                .program_runtime
                                .hydrate_reducible_state_if_newer(
                                    bootstrap.checkpoint,
                                    bootstrap.runtime_revision,
                                )
                                .await
                            {
                                Ok(_) => {
                                    self.agent_scheduler
                                        .bind_brain_control(bootstrap.subagent_control)
                                        .await;
                                    Ok(())
                                }
                                Err(error) => {
                                    let _ = ipc.brain_release_runner(&brain, lease_id).await;
                                    self.agent_scheduler.clear_brain_control().await;
                                    Err(error.to_string())
                                }
                            }
                        }
                        Err(error) => Err(error.to_string()),
                    },
                    (Some(_), None) => Err("Cap'n Proto daemon connection unavailable".into()),
                    (None, _) => Err(detail.clone()),
                };
                let active = registration.is_ok();
                if !active {
                    self.agent_scheduler.clear_brain_control().await;
                }
                self.home_runner_lease_active = active;
                // Registration loss does not erase the durable lease identity;
                // it is precisely what a replacement connection must reclaim.
                self.runner_brain = active.then_some(brain.clone());
                let registration_error = registration.err();
                let handed_off = !active && detail.contains("handed off");
                self.home_runner_lease_id = lease_id_after_registration(
                    self.home_runner_lease_id,
                    lease_id,
                    active,
                    handed_off,
                );
                let reconnect_target = RunnerReconnectTarget {
                    brain: brain.clone(),
                    environment,
                    lease_id: self.home_runner_lease_id,
                };
                self.runner_reconnect_target = (!handed_off).then(|| reconnect_target.clone());
                if !active && !handed_off {
                    self.schedule_home_runner_reconnect(epoch, 0, reconnect_target);
                }
                if self.active_remote_brain.is_none() {
                    if self.home_brain.is_some() {
                        self.update_remote_brain_status(active);
                    } else {
                        self.status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::SessionLabel,
                            if active {
                                format!("◆ brain: {} · runner", brain)
                            } else {
                                format!("◆ brain: {} · home · no runner lease", self.session_label)
                            },
                        );
                    }
                    if let Some(error) = registration_error {
                        let changed = self.last_home_runner_error.as_deref() != Some(&error);
                        self.last_home_runner_error = Some(error.clone());
                        if changed {
                            self.output_manager.write_info(format!(
                                "{}: runner unavailable: {}",
                                self.session_label, error
                            ));
                        }
                    } else {
                        self.last_home_runner_error = None;
                    }
                    self.render_tui().await?;
                }
            }
            ReplEvent::NamedBrainProgramRequested(request) => {
                self.dispatch_named_brain_program(request);
            }
            ReplEvent::NamedBrainTurnRequested(request) => {
                self.dispatch_named_brain_turn(request).await?;
            }
            ReplEvent::NamedBrainMemoryProjectionRequested(request) => {
                self.project_named_brain_memory(request).await;
            }
            ReplEvent::NamedBrainRunCancelRequested(request) => {
                self.cancel_named_brain_run(request).await;
            }
            ReplEvent::NamedBrainProgramFinished(run_id) => {
                self.pending_named_brain_programs.remove(&run_id);
            }
            ReplEvent::FrontendRestartReady {
                brain,
                run_id,
                restart,
            } => {
                if let Err(error) = self
                    .restart_frontend_after_brain_commit(brain, run_id, restart)
                    .await
                {
                    self.output_manager
                        .write_error(format!("Frontend restart failed: {error:#}"));
                    self.render_tui().await?;
                }
            }
            ReplEvent::ShowDialog {
                dialog: _,
                response_tx,
            } => {
                // active_dialog is already set by the caller (belt-and-suspenders in
                // handle_present_plan / handle_ask_user_question), so the dialog is
                // on-screen before the event is even enqueued — no race window.
                // Just store the response channel for the render tick to route the result.
                self.pending_dialog_tx = Some(response_tx);
            }
        }

        Ok(())
    }
}
