use super::*;

impl EventLoop {
    #[cfg(test)]
    pub(crate) fn new_named_brain_test_runner(
        generator: Arc<dyn Generator>,
        tool_definitions: Vec<ToolDefinition>,
        tool_executor: Arc<Mutex<ToolExecutor>>,
        program_runtime: Arc<crate::runtime::ProgramRuntime>,
    ) -> Self {
        let colors = crate::theme::ColorScheme::default();
        let output_manager = Arc::new(OutputManager::new(colors.clone()));
        let status_bar = Arc::new(StatusBar::new());
        let tui_renderer =
            TuiRenderer::new_headless(Arc::clone(&output_manager), Arc::clone(&status_bar), colors);
        let todo_list = Arc::new(tokio::sync::RwLock::new(
            crate::tools::todo::TodoList::default(),
        ));
        let (_todo_writer, todo_target, todo_receiver) =
            crate::tools::todo::todo_journal(Arc::clone(&todo_list));
        let provider_resolver = crate::scheduler::ProviderResolver::new(Arc::clone(&generator));
        let agent_scheduler = crate::scheduler::AgentScheduler::new(
            provider_resolver.clone(),
            Arc::clone(&program_runtime),
        );
        Self::new(
            Arc::new(RwLock::new(ConversationHistory::new())),
            Arc::new(RwLock::new(crate::config::Persona::default())),
            Arc::clone(&generator),
            generator,
            Arc::new(Router::new(crate::models::ThresholdRouter::new())),
            Arc::new(RwLock::new(GeneratorState::NotAvailable)),
            tool_definitions,
            tool_executor,
            program_runtime,
            tui_renderer,
            output_manager,
            status_bar,
            false,
            Arc::new(RwLock::new(crate::local::LocalGenerator::new())),
            Arc::new(crate::models::TextTokenizer::stub().expect("stub tokenizer")),
            None,
            None,
            Arc::new(RwLock::new(ReplMode::Normal)),
            None,
            "audit-test".into(),
            Uuid::new_v4(),
            Vec::new(),
            0,
            None,
            0,
            0,
            0,
            todo_list,
            todo_target,
            todo_receiver,
            false,
            false,
            None,
            provider_resolver,
            agent_scheduler,
        )
    }

    pub(super) fn dispatch_named_brain_program(
        &mut self,
        request: crate::server::RunnerProgramRequest,
    ) {
        if self.runner_brain.as_deref() != Some(request.brain.as_str())
            || !self.home_runner_lease_active
        {
            let _ = request.response_tx.send(Err(format!(
                "frontend does not hold the runner lease for named Brain '{}'",
                request.brain
            )
            .into()));
            return;
        }
        let runtime = Arc::clone(&self.program_runtime);
        let agent_scheduler = Arc::clone(&self.agent_scheduler);
        let event_tx = self.event_tx.clone();
        let run_id = request.run_id;
        let request_seq = request.request_seq;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.pending_named_brain_programs
            .insert(run_id, cancel.clone());
        tokio::task::spawn_local(async move {
            agent_scheduler
                .set_active_brain_parent(Some(crate::scheduler::AgentBrainContext {
                    run_id,
                    request_seq,
                }))
                .await;
            let brain_language = request.language;
            let language = match brain_language {
                crate::brain::store::ProgramLanguage::Forth => {
                    crate::programs::ProgramLanguage::Forth
                }
                crate::brain::store::ProgramLanguage::Lisp => {
                    crate::programs::ProgramLanguage::Lisp
                }
            };
            let submission = crate::runtime::ProgramSubmission {
                language,
                source_id: Some(format!(
                    "brain:{}:event:{}",
                    request.brain, request.request_seq
                )),
                source: request.source,
                intent: format!("named Brain program event {}", request.request_seq),
                effect: crate::programs::ExecutionEffect::Unclassified,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            };
            let execution = async {
                let fixed_grant_ceiling = request.grant_ceiling.clone();
                let (effect_sink, effect_rx) = crate::runtime::typed_effect_channel();
                let outcome = runtime
                    .submit_typed_only_with_deferred_schedule_effects(
                        submission,
                        effect_sink,
                        fixed_grant_ceiling.clone(),
                        request.effect_audit.clone(),
                    )
                    .await
                    .map_err(|error| crate::server::RunnerProgramError::from(error.to_string()))?;
                let execution_id = outcome.execution_id;
                let mut resumed = Box::pin(async {
                    resume_named_brain_program_boundaries(
                        runtime.as_ref(),
                        event_tx.clone(),
                        request.control_tx,
                        brain_language,
                        request.interaction,
                        fixed_grant_ceiling,
                        effect_rx,
                        outcome,
                    )
                    .await
                });
                let outcome = tokio::select! {
                    biased;
                    result = &mut resumed => result.map_err(|error| {
                        crate::server::RunnerProgramError::from(error.to_string())
                    })?,
                    _ = cancel.cancelled() => {
                        match runtime
                            .cancel_typed_execution_with_outcome(execution_id)
                            .map_err(|error| crate::server::RunnerProgramError::from(error.to_string()))?
                        {
                            Some(cancelled) => {
                                return Err(crate::server::RunnerProgramError {
                                    message: "named Brain run cancelled".into(),
                                    effect_journal: crate::cli::repl_event::query_processor::runner_effect_records(
                                        &cancelled,
                                    ),
                                });
                            }
                            None => resumed.await.map_err(|error| {
                                crate::server::RunnerProgramError::from(error.to_string())
                            })?,
                        }
                    }
                };
                let effect_journal =
                    crate::cli::repl_event::query_processor::runner_effect_records(&outcome);
                if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
                    return Err(crate::server::RunnerProgramError {
                        message: format!(
                            "named Brain ProgramRun ended as {:?}: {}",
                            outcome.status,
                            outcome.diagnostics.join("; ")
                        ),
                        effect_journal,
                    });
                }
                let checkpoint = runtime
                    .revision_history()
                    .map_err(|error| crate::server::RunnerProgramError::from(error.to_string()))?
                    .into_iter()
                    .find(|snapshot| snapshot.revision == outcome.output_revision)
                    .and_then(|snapshot| snapshot.checkpoint)
                    .ok_or_else(|| crate::server::RunnerProgramError {
                        message: format!(
                            "named Brain revision {} is not checkpointable",
                            outcome.output_revision
                        ),
                        effect_journal: effect_journal.clone(),
                    })?;
                Ok(crate::server::RunnerProgramResult {
                    output: outcome.output,
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                    effect_journal,
                })
            };
            let result = execution.await;
            agent_scheduler.set_active_brain_parent(None).await;
            let _ = request.response_tx.send(result);
            let _ = event_tx.send(ReplEvent::NamedBrainProgramFinished(run_id));
        });
    }

    pub(super) async fn dispatch_named_brain_turn(
        &mut self,
        request: crate::server::RunnerTurnRequest,
    ) -> Result<()> {
        if self.runner_brain.as_deref() != Some(request.brain.as_str())
            || !self.home_runner_lease_active
        {
            let _ = request
                .response_tx
                .send(Err(crate::server::RunnerTurnError {
                    message: format!(
                        "frontend does not hold the runner lease for named Brain '{}'",
                        request.brain
                    ),
                    turn_events: Vec::new(),
                    effect_journal: Vec::new(),
                }));
            return Ok(());
        }
        if self.active_query_id.read().await.is_some() {
            let _ = request
                .response_tx
                .send(Err(crate::server::RunnerTurnError {
                    message: format!(
                        "named Brain '{}' runner is already executing a turn",
                        request.brain
                    ),
                    turn_events: Vec::new(),
                    effect_journal: Vec::new(),
                }));
            return Ok(());
        }

        let mut context = request.context;
        if context.is_empty() {
            context.push(crate::claude::Message::user(request.prompt.clone()));
        }
        self.conversation
            .write()
            .await
            .restore_snapshot(context.clone());
        let query_id = self.query_states.create_query(context).await;
        self.query_states
            .bind_brain_turn_provenance(
                query_id,
                crate::cli::repl_event::query_state::BrainTurnProvenance {
                    brain_id: request.approval_audience.brain_id,
                    run_id: request.run_id,
                    request_seq: request.request_seq,
                },
            )
            .await;
        let mut effect_audit = request.effect_audit;
        #[cfg(test)]
        if let Some(wrapper) = &self.effect_audit_test_wrapper {
            effect_audit = effect_audit.map(|control| wrapper(control));
        }
        if let Some(effect_audit) = effect_audit.clone() {
            self.query_states
                .bind_effect_audit(query_id, effect_audit)
                .await;
        }
        let run_unit = self
            .ensure_remote_brain_run_projection(
                request.run_id,
                None,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unit
            .clone();
        self.query_states
            .set_tool_work_unit(query_id, Some(run_unit))
            .await;
        *self.active_query_id.write().await = Some(query_id);
        self.pending_named_brain_turns.insert(
            query_id,
            PendingNamedBrainTurn {
                brain: request.brain,
                run_id: request.run_id,
                response_tx: request.response_tx,
                turn_events: Vec::new(),
                effect_journal: Vec::new(),
                cancellation_requested: false,
                active_tool_ids: std::collections::HashSet::new(),
                approval_audience: request.approval_audience,
                approval_tx: request.approval_tx,
                effect_audit,
                restart: None,
            },
        );
        self.agent_scheduler
            .set_active_brain_parent(Some(crate::scheduler::AgentBrainContext {
                run_id: request.run_id,
                request_seq: request.request_seq,
            }))
            .await;
        self.update_compaction_status().await;
        if self
            .llm_tx
            .send(LlmRequest::Query {
                id: query_id,
                text: request.prompt,
                no_tools: false,
                admission: None,
                admission_ready: None,
                spawned: None,
                publication: None,
            })
            .is_err()
        {
            *self.active_query_id.write().await = None;
            self.agent_scheduler.set_active_brain_parent(None).await;
            if let Some(pending) = self.pending_named_brain_turns.remove(&query_id) {
                let _ = pending
                    .response_tx
                    .send(Err(crate::server::RunnerTurnError {
                        message: "frontend LLM worker is unavailable".to_string(),
                        turn_events: pending.turn_events,
                        effect_journal: pending.effect_journal,
                    }));
            }
        }
        Ok(())
    }

    pub(super) async fn finish_named_brain_turn(&mut self, query_id: Uuid, output: String) {
        let query_metadata = self.query_states.get_metadata(query_id).await;
        let invocation_metadata = query_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation_metadata.clone());
        let initial_message_count = query_metadata
            .as_ref()
            .map_or(0, |metadata| metadata.conversation_snapshot.len());
        let transient_output_unit = self.query_states.brain_output_work_unit(query_id).await;
        self.query_states.set_tool_work_unit(query_id, None).await;
        self.query_states
            .set_brain_output_work_unit(query_id, None)
            .await;
        let Some(pending) = self.pending_named_brain_turns.remove(&query_id) else {
            return;
        };
        self.agent_scheduler.set_active_brain_parent(None).await;
        let PendingNamedBrainTurn {
            brain,
            run_id,
            response_tx,
            turn_events,
            effect_journal,
            cancellation_requested,
            effect_audit,
            restart,
            ..
        } = pending;
        // Retain the opaque run-scoped capability until the provider and all
        // launched tools have crossed their terminal boundary. Dropping it
        // earlier would prevent an already-begun detached effect from filing
        // its one authoritative late outcome.
        drop(effect_audit);
        if cancellation_requested {
            let _ = response_tx.send(Err(crate::server::RunnerTurnError {
                message: "named Brain run cancelled".into(),
                turn_events,
                effect_journal,
            }));
            return;
        }
        let commit_ack = restart.map(|restart| {
            let (commit_tx, mut commit_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::server::RunnerTurnCommitNotice>();
            let event_tx = self.event_tx.clone();
            tokio::spawn(async move {
                let Some(notice) = commit_rx.recv().await else {
                    return;
                };
                if notice.status == crate::brain::store::BrainRunStatus::Completed {
                    let _ = event_tx.send(ReplEvent::FrontendRestartReady {
                        brain,
                        run_id,
                        restart,
                    });
                } else {
                    let _ = event_tx.send(ReplEvent::OutputReady {
                        message: format!(
                            "Frontend restart cancelled because Brain run {} ended as {:?}: {}",
                            run_id.0, notice.status, notice.detail
                        ),
                    });
                }
            });
            crate::server::RunnerTurnCommitAck::new(commit_tx)
        });
        let messages = self
            .conversation
            .try_read()
            .map(|conversation| conversation.get_messages())
            .map_err(|_| anyhow::anyhow!("named Brain conversation is busy"));
        let result = assemble_named_brain_turn(
            &mut self.local_brain_projections,
            run_id,
            messages,
            self.program_runtime.as_ref(),
            output,
            turn_events,
            effect_journal,
            commit_ack,
            transient_output_unit,
            invocation_metadata,
            initial_message_count,
        );
        let _ = response_tx.send(result);
    }

    /// Replace this frontend only after the daemon has acknowledged the
    /// canonical turn as complete. The old callback lease is released
    /// explicitly so the replacement can immediately acquire the same Brain;
    /// no legacy conversation file participates in restoration.
    pub(super) async fn restart_frontend_after_brain_commit(
        &mut self,
        brain: String,
        run_id: crate::brain::store::RunId,
        restart: crate::tools::implementations::restart::DeferredFrontendRestart,
    ) -> Result<()> {
        anyhow::ensure!(
            self.runner_brain.as_deref() == Some(brain.as_str()) && self.home_runner_lease_active,
            "cannot restart after Brain run {}: this frontend no longer owns '{}'",
            run_id.0,
            brain
        );
        let lease_id = self
            .home_runner_lease_id
            .context("cannot restart the frontend without its exact runner lease identity")?;
        let ipc = self
            .ipc_client
            .as_ref()
            .context("cannot restart the frontend without the Cap'n Proto daemon connection")?
            .clone();
        let environment = ipc
            .brain_snapshot(&brain)
            .await
            .with_context(|| format!("inspect Brain '{brain}' before frontend restart"))?
            .environment;

        // Check both the approved digest and basic loadability before giving
        // up the callback authority that can recover this Brain.
        restart.preflight()?;
        self.output_manager.write_info(format!(
            "Brain run {} committed; restarting this frontend with {} ({})",
            run_id.0,
            restart.binary_path.display(),
            restart.reason
        ));
        self.render_tui().await?;

        self.runner_renewal_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ipc.brain_release_runner(&brain, lease_id),
        )
        .await
        .context("timed out releasing the current Brain runner lease")??;
        self.home_runner_lease_active = false;
        self.home_runner_lease_id = None;
        self.runner_brain = None;
        self.runner_reconnect_target = None;
        self.agent_scheduler.clear_brain_control().await;

        // Recheck after the asynchronous lease release narrows the replacement
        // race. A fully race-free future implementation can exec an already
        // opened descriptor on platforms that support it.
        if let Err(error) = restart.verify() {
            return Err(self
                .fail_handoff_and_restore_runner(
                    &ipc,
                    Some((brain.clone(), environment.clone())),
                    error.context("restart candidate verification after lease release"),
                )
                .await);
        }
        let args = crate::tools::implementations::restart::frontend_replacement_args(
            std::env::args_os(),
            &brain,
        );
        crate::set_tui_active(false);
        crate::cli::tui::emergency_restore_terminal();

        let mut command = std::process::Command::new(&restart.binary_path);
        command.args(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = command.exec();
            let error = anyhow::anyhow!(
                "failed to exec restart candidate '{}': {}",
                restart.binary_path.display(),
                error
            );
            let error = self
                .fail_handoff_and_restore_runner(&ipc, Some((brain, environment)), error)
                .await;
            crate::set_tui_active(true);
            self.tui_renderer
                .lock()
                .await
                .resume_after_emergency_restore()
                .context("restore terminal after failed frontend exec")?;
            return Err(error);
        }
        #[cfg(not(unix))]
        {
            command.spawn().with_context(|| {
                format!(
                    "failed to start restart candidate '{}'",
                    restart.binary_path.display()
                )
            })?;
            std::process::exit(0);
        }
    }

    pub(super) async fn handle_brain_attach_with_invitation(
        &mut self,
        value: String,
        invitation: Option<String>,
    ) -> Result<()> {
        let route = match brain_attachment_route(&value, invitation) {
            Ok(route) => route,
            Err(error) => {
                self.output_manager
                    .write_info(format!("brain attach: {error}"));
                self.render_tui().await?;
                return Ok(());
            }
        };
        let (target, mut client, invited) = match route {
            BrainAttachmentRoute::LocalIpc { brain } => {
                let ipc = self
                    .ipc_client
                    .clone()
                    .context("local Brain attachment requires the connected daemon IPC socket")?;
                let mut target = self
                    .home_brain
                    .as_ref()
                    .map(|home| home.target.clone())
                    .context("this console has no local Brain environment")?;
                target.brain = brain;
                (
                    target.clone(),
                    crate::brain::remote::AttachedBrainClient::local(target, ipc),
                    false,
                )
            }
            BrainAttachmentRoute::RemoteInvitation { target, invitation } => {
                let remote = crate::brain::remote::RemoteBrainClient::new_with_invitation(
                    target.clone(),
                    invitation,
                )?;
                (
                    target,
                    crate::brain::remote::AttachedBrainClient::remote(remote),
                    true,
                )
            }
        };
        if self.home_brain.as_ref().is_some_and(|home| {
            home.target.brain == target.brain && home.target.address == target.address
        }) {
            self.output_manager.write_info(format!(
                "{} is already this console's home Brain",
                target.display_name()
            ));
            return self.render_tui().await;
        }
        let attachment = if invited {
            client
                .attach_invited_persistent(&self.participant_subject, &self.session_label)
                .await
                .map(|(_, attachment)| attachment)
        } else {
            client
                .attach_persistent(
                    &self.participant_subject,
                    crate::brain::store::AttachmentRole::Driver,
                    &self.session_label,
                )
                .await
        };
        if let Err(error) = attachment {
            self.output_manager.write_info(format!(
                "brain attach {}: {error:#}",
                client.target.display_name()
            ));
            self.render_tui().await?;
            return Ok(());
        }
        let mut incoming = match client.watch().await {
            Ok(incoming) => incoming,
            Err(error) => {
                let _ = client.disconnect().await;
                self.output_manager.write_info(format!(
                    "brain attach {}: {error}",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };
        let snapshot = match incoming.recv().await {
            Some(crate::brain::store::BrainWireMessage::Snapshot { brain }) => brain,
            Some(crate::brain::store::BrainWireMessage::Event { .. }) => {
                self.output_manager.write_info(format!(
                    "brain attach {}: event stream did not begin with a snapshot",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
            None => {
                self.output_manager.write_info(format!(
                    "brain attach {}: event stream closed before its snapshot",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };
        client.target.machine = snapshot.environment.machine.clone();

        let target_name = client.target.display_name();
        let runner_online = snapshot.runner_lease.is_some();
        self.active_remote_brain = Some(client);
        self.todo_journal_target
            .set(self.active_remote_brain.clone());
        self.update_remote_brain_status(runner_online);
        self.render_remote_brain_message(crate::brain::store::BrainWireMessage::Snapshot {
            brain: snapshot,
        })
        .await?;

        let event_tx = self.event_tx.clone();
        tokio::task::spawn_local(async move {
            while let Some(message) = incoming.recv().await {
                if event_tx
                    .send(ReplEvent::RemoteBrainMessage {
                        target: target_name.clone(),
                        message,
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = event_tx.send(ReplEvent::RemoteBrainDisconnected {
                target: target_name,
            });
        });
        Ok(())
    }

    pub(super) async fn handle_brain_invite(
        &mut self,
        role: String,
        ttl_minutes: Option<u64>,
    ) -> Result<()> {
        if self.active_remote_brain.is_some() {
            anyhow::bail!(
                "issue invitations from the Brain owner's home console, not through a guest attachment"
            );
        }
        let role = match role.to_ascii_lowercase().as_str() {
            "driver" => crate::brain::store::AttachmentRole::Driver,
            "consultant" => crate::brain::store::AttachmentRole::Consultant,
            "observer" => crate::brain::store::AttachmentRole::Observer,
            _ => anyhow::bail!("role must be driver, consultant, or observer"),
        };
        let home = self
            .home_brain
            .as_ref()
            .context("this console has no home Brain")?;
        let daemon_base_url = self
            .daemon_base_url
            .as_deref()
            .context("this console is not connected to its local daemon")?;
        let target =
            crate::brain::remote::RemoteBrainTarget::local(&home.target.brain, daemon_base_url)?;
        let config = crate::config::load_config().context("load Brain collaboration settings")?;
        anyhow::ensure!(
            config.server.advertise,
            "remote Brain collaboration is disabled; enable LAN discovery/advertisement before issuing an invitation"
        );
        let recipient_target = crate::brain::remote::RemoteBrainTarget::invitation_recipient(
            &home.target.brain,
            &home.target.machine,
            &config.server.brain_bind_address,
        )?;
        let password = config.server.brain_password;
        let client = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        let ttl_ms = ttl_minutes
            .map(|minutes| {
                minutes
                    .checked_mul(60_000)
                    .context("invitation lifetime is too large")
            })
            .transpose()?;
        let (invitation, claims) = client.issue_invitation(role, ttl_ms).await?;
        let invitation_client = crate::brain::remote::RemoteBrainClient::new_with_invitation(
            recipient_target.clone(),
            invitation.clone(),
        )?;
        invitation_client.probe_invitation_endpoint().await?;
        self.output_manager.write_info(format!(
            "Brain invitation for {} ({}, expires at Unix ms {}):\n{}\n\nRecipient command:\n/brain join {} {}",
            claims.brain,
            format!("{:?}", claims.role).to_ascii_lowercase(),
            claims.expires_ms,
            invitation,
            recipient_target.command_target(),
            invitation,
        ));
        self.render_tui().await
    }

    pub(super) async fn render_remote_brain_message(
        &mut self,
        message: crate::brain::store::BrainWireMessage,
    ) -> Result<()> {
        match message {
            crate::brain::store::BrainWireMessage::Snapshot { brain } => {
                self.update_remote_brain_status(brain.runner_lease.is_some());
                let local_machine = self
                    .selected_brain()
                    .is_some_and(|client| !client.target.secure)
                    .then_some(brain.environment.machine.as_str());
                let selected_brain_is_home = self.selected_brain_is_home();
                project_remote_brain_snapshot_runs(
                    &self.output_manager,
                    &mut self.remote_brain_run_units,
                    &mut self.local_brain_projections,
                    selected_brain_is_home,
                    &brain.events,
                );
                self.todo_list
                    .write()
                    .await
                    .replace_all(brain.tasks.clone());
                project_brain_context(
                    &self.status_bar,
                    &brain.events,
                    self.context_lines.saturating_sub(1),
                    local_machine,
                );
                let acknowledged_seq = self
                    .selected_brain()
                    .and_then(|client| client.attachment())
                    .map(|attachment| attachment.acknowledged_seq)
                    .unwrap_or(0);
                for event in brain
                    .events
                    .iter()
                    .filter(|event| event.seq > acknowledged_seq)
                {
                    if event.run_id.is_none() && replay_event_belongs_in_transcript(event) {
                        self.render_remote_brain_event(event).await;
                    }
                    self.observe_remote_brain_approval(event);
                }
                advance_brain_projection_revision(
                    &mut self.brain_projection_revisions,
                    brain.brain_id,
                    brain.revision,
                );
            }
            crate::brain::store::BrainWireMessage::Event { event } => {
                if !advance_brain_projection_revision(
                    &mut self.brain_projection_revisions,
                    event.brain_id,
                    event.seq,
                ) {
                    return Ok(());
                }
                match &event.kind {
                    crate::brain::store::BrainEventKind::RunnerLeaseAcquired { .. } => {
                        self.update_remote_brain_status(true);
                    }
                    crate::brain::store::BrainEventKind::RunnerLeaseReleased { .. } => {
                        self.update_remote_brain_status(false);
                    }
                    _ => {}
                }
                if brain_context_text(&event, None).is_some() {
                    if let Some(client) = self.selected_brain().cloned() {
                        if let Ok(snapshot) = client.snapshot().await {
                            let local_machine = (!client.target.secure)
                                .then_some(snapshot.environment.machine.as_str());
                            project_brain_context(
                                &self.status_bar,
                                &snapshot.events,
                                self.context_lines.saturating_sub(1),
                                local_machine,
                            );
                        }
                    }
                }
                self.render_remote_brain_event(&event).await;
                self.observe_remote_brain_approval(&event);
            }
        }
        self.try_present_remote_brain_approval().await?;
        self.render_tui().await
    }

    pub(super) fn observe_remote_brain_approval(
        &mut self,
        event: &crate::brain::store::BrainEvent,
    ) {
        use crate::brain::store::BrainEventKind;

        match &event.kind {
            BrainEventKind::ApprovalRequested {
                request_seq,
                approval_id,
                approval_kind,
                subject,
                audience: Some(audience),
                detail,
            } => {
                let Some(client) = self.selected_brain().cloned() else {
                    return;
                };
                if client
                    .attachment()
                    .is_none_or(|attachment| attachment.attachment_id != audience.attachment_id)
                {
                    return;
                }
                if self
                    .active_remote_brain_approval
                    .as_ref()
                    .is_some_and(|pending| pending.approval_id == *approval_id)
                    || self
                        .queued_remote_brain_approvals
                        .iter()
                        .any(|pending| pending.approval_id == *approval_id)
                {
                    return;
                }
                let kind = match approval_kind.as_str() {
                    "tool" => RemoteBrainApprovalKind::Tool(crate::tools::types::ToolUse {
                        id: approval_id.clone(),
                        name: subject.clone(),
                        input: detail
                            .get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    }),
                    "vm_capability" => {
                        let Ok(prompt) =
                            serde_json::from_value::<crate::vm::ApprovalPrompt>(detail.clone())
                        else {
                            self.output_manager.write_error(format!(
                                "approval {approval_id} has an invalid VM capability prompt"
                            ));
                            return;
                        };
                        let choices = vm_approval_choices(&prompt);
                        RemoteBrainApprovalKind::Vm { prompt, choices }
                    }
                    _ => {
                        self.output_manager.write_error(format!(
                            "approval {approval_id} has unknown kind '{approval_kind}'"
                        ));
                        return;
                    }
                };
                self.queued_remote_brain_approvals
                    .push_back(RemoteBrainApproval {
                        client,
                        request_seq: *request_seq,
                        approval_id: approval_id.clone(),
                        audience: audience.clone(),
                        kind,
                    });
            }
            BrainEventKind::ApprovalDecided { approval_id, .. } => {
                self.queued_remote_brain_approvals
                    .retain(|pending| pending.approval_id != *approval_id);
                if self
                    .active_remote_brain_approval
                    .as_ref()
                    .is_some_and(|pending| pending.approval_id == *approval_id)
                {
                    self.active_remote_brain_approval = None;
                }
            }
            _ => {}
        }
    }

    pub(super) async fn render_remote_brain_event(
        &mut self,
        event: &crate::brain::store::BrainEvent,
    ) {
        use crate::brain::store::BrainEventKind;
        let selected_brain_is_home = self.selected_brain_is_home();
        if project_remote_brain_live_run_event(
            &self.output_manager,
            &mut self.remote_brain_run_units,
            &mut self.local_brain_projections,
            selected_brain_is_home,
            event,
        ) {
            return;
        }
        let local_machine = self
            .selected_brain()
            .filter(|client| !client.target.secure)
            .map(|client| client.target.machine.as_str());
        let sender = participant_display_name(&event.sender, local_machine);
        match &event.kind {
            BrainEventKind::MutationRecorded { .. } => {}
            BrainEventKind::RunnerLeaseAcquired { lease } => self.output_manager.write_info(
                format!("{} is the active environment runner", lease.subject),
            ),
            BrainEventKind::RunnerLeaseReleased { .. } => self
                .output_manager
                .write_info("environment runner disconnected"),
            BrainEventKind::RunnerHandoffRequested { handoff } => {
                let prompt = if handoff.target_subject == self.runner_subject {
                    format!(
                        "; addressed to this frontend — use /brain handoff accept {}",
                        &handoff.handoff_id.0.to_string()[..8]
                    )
                } else {
                    String::new()
                };
                self.output_manager.write_info(format!(
                    "{} requested runner handoff to {}{}",
                    handoff.requested_by, handoff.target_subject, prompt
                ));
            }
            BrainEventKind::RunnerHandoffCompleted { lease, .. } => self
                .output_manager
                .write_info(format!("runner handoff completed to {}", lease.subject)),
            BrainEventKind::RunnerHandoffCancelled { .. } => {
                self.output_manager.write_info("runner handoff cancelled")
            }
            BrainEventKind::ClientAttached { subject, role, .. } => {
                self.output_manager.write_info(format!(
                    "{subject} attached as {}",
                    format!("{role:?}").to_lowercase()
                ))
            }
            BrainEventKind::ClientDetached { attachment_id, .. } => self
                .output_manager
                .write_info(format!("attachment {} disconnected", attachment_id.0)),
            BrainEventKind::RunStarted { run } => {
                self.ensure_remote_brain_run_projection(run.run_id, Some(run.kind), run.status);
            }
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            } => {
                let projection = self.ensure_remote_brain_run_projection(*run_id, None, *status);
                let summary = detail
                    .as_deref()
                    .map(|detail| format!("{}: {detail}", format!("{status:?}").to_lowercase()))
                    .unwrap_or_else(|| format!("{status:?}").to_lowercase());
                if *status == crate::brain::store::BrainRunStatus::Failed {
                    projection.unit.fail_row(projection.status_row, summary);
                } else {
                    projection.unit.complete_row(projection.status_row, summary);
                }
                if status.is_terminal() {
                    projection.unit.set_complete();
                }
            }
            BrainEventKind::Prompt { text } => {
                self.output_manager
                    .write_brain_participant(sender.clone(), text.clone(), true)
            }
            BrainEventKind::SpeculativePrompt { text } => {
                if let Some(run_id) = event.run_id {
                    let projection = self.ensure_remote_brain_run_projection(
                        run_id,
                        Some(crate::brain::store::BrainRunKind::Speculative),
                        crate::brain::store::BrainRunStatus::QueuedForEnvironment,
                    );
                    let row = projection.unit.add_activity_row("prompt");
                    projection.unit.complete_row_with_body(
                        row,
                        "accepted",
                        text.lines().map(str::to_owned).collect(),
                    );
                } else {
                    self.output_manager.write_brain_participant(
                        sender.clone(),
                        format!("[legacy speculative] {text}"),
                        false,
                    );
                }
            }
            BrainEventKind::ParticipantMessage { text } => self
                .output_manager
                .write_brain_participant(sender, text.clone(), false),
            BrainEventKind::TaskListReplaced { tasks } => {
                self.todo_list.write().await.replace_all(tasks.clone());
            }
            BrainEventKind::ToolCall {
                tool_id,
                name,
                input,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let input = input.to_string();
                let input = if input.chars().count() > 80 {
                    format!("{}…", input.chars().take(79).collect::<String>())
                } else {
                    input
                };
                let row = unit.add_row(format!("{name} {input}"));
                self.remote_brain_tool_rows.insert(tool_id.clone(), row);
            }
            BrainEventKind::ToolResult {
                tool_id,
                output,
                is_error,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let row = self
                    .remote_brain_tool_rows
                    .remove(tool_id)
                    .unwrap_or_else(|| unit.add_row(tool_id));
                if *is_error {
                    unit.fail_row(row, output);
                } else {
                    let first = output.lines().next().unwrap_or_default();
                    let summary = if first.chars().count() > 80 {
                        format!("{}…", first.chars().take(79).collect::<String>())
                    } else {
                        first.to_string()
                    };
                    let body = output.lines().skip(1).map(str::to_owned).collect();
                    unit.complete_row_with_body(row, summary, body);
                }
            }
            BrainEventKind::ApprovalRequested {
                approval_id,
                approval_kind,
                subject,
                audience,
                detail,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let audience_summary = audience
                    .as_ref()
                    .map(|audience| {
                        format!(
                            "{} ({:?}, environment {})",
                            audience.subject, audience.role, audience.environment_generation
                        )
                    })
                    .unwrap_or_else(|| "legacy audience unspecified".to_string());
                let row = unit.add_row(format!(
                    "approval ({approval_kind}) for {audience_summary}: {subject}"
                ));
                let body = serde_json::to_string_pretty(detail)
                    .unwrap_or_else(|_| detail.to_string())
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                for line in body {
                    unit.append_row_body_line(row, line);
                }
                self.remote_brain_approval_rows
                    .insert(approval_id.clone(), row);
            }
            BrainEventKind::ApprovalDecided {
                approval_id,
                decision,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let row = self
                    .remote_brain_approval_rows
                    .remove(approval_id)
                    .unwrap_or_else(|| unit.add_row(format!("approval {approval_id}")));
                let choice = decision
                    .get("choice")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("decided");
                let summary = format!("{choice} by {}", event.sender);
                if choice == "deny" {
                    unit.fail_row(row, &summary);
                } else {
                    unit.complete_row(row, &summary);
                }
            }
            BrainEventKind::Program { language, source } => {
                if let Some(unit) = self.remote_brain_tool_unit.take() {
                    unit.set_complete();
                }
                self.remote_brain_tool_rows.clear();
                self.remote_brain_approval_rows.clear();
                let locally_projected = self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        });
                if let Some(run_id) = event.run_id {
                    let projection = self.ensure_remote_brain_run_projection(
                        run_id,
                        None,
                        crate::brain::store::BrainRunStatus::Running,
                    );
                    let language = match language {
                        crate::brain::store::ProgramLanguage::Forth => "Co-Forth",
                        crate::brain::store::ProgramLanguage::Lisp => "Lisp",
                    };
                    let row = projection.program_row.unwrap_or_else(|| {
                        projection
                            .unit
                            .add_activity_row(format!("{language} program"))
                    });
                    projection.program_row = Some(row);
                    projection.unit.complete_row_with_body(
                        row,
                        format!("event #{}", event.seq),
                        source.lines().map(str::to_owned).collect(),
                    );
                    return;
                }
                if locally_projected {
                    return;
                }
                let language = match language {
                    crate::brain::store::ProgramLanguage::Forth => "forth",
                    crate::brain::store::ProgramLanguage::Lisp => "lisp",
                };
                let unit = self
                    .output_manager
                    .start_work_unit(format!("{} program", event.sender));
                unit.set_program_source(language);
                unit.set_response(source);
                unit.set_complete();
            }
            BrainEventKind::ProgramPopped { program_seq } => self
                .output_manager
                .write_info(format!("{} popped program #{program_seq}", event.sender)),
            BrainEventKind::Result {
                request_seq: _,
                output,
                error,
                ..
            } => {
                let projection_match = self
                    .selected_brain_is_home()
                    .then(|| self.local_brain_projections.front_mut())
                    .flatten()
                    .map(|projection| projection.observe(event))
                    .unwrap_or(LocalProjectionMatch::None);
                if let Some(run_id) = event.run_id {
                    let projection = self.ensure_remote_brain_run_projection(
                        run_id,
                        None,
                        crate::brain::store::BrainRunStatus::Running,
                    );
                    let row = projection.unit.add_activity_row("result");
                    if let Some(error) = error {
                        projection.unit.fail_row(row, error);
                    } else {
                        projection.unit.complete_row_with_body(
                            row,
                            "completed",
                            output.lines().map(str::to_owned).collect(),
                        );
                    }
                    if projection_match == LocalProjectionMatch::SuppressAndComplete {
                        self.local_brain_projections.pop_front();
                    }
                    return;
                }
                if projection_match == LocalProjectionMatch::SuppressAndComplete {
                    self.local_brain_projections.pop_front();
                    return;
                }
                if let Some(error) = error {
                    self.output_manager.write_info(format!("error: {error}"));
                } else if !output.is_empty() {
                    let label = event
                        .run_id
                        .map(|run_id| format!("Brain run {} output", &run_id.0.to_string()[..8]))
                        .unwrap_or_else(|| "Brain program output".to_string());
                    let unit = self.output_manager.start_work_unit(label);
                    unit.set_program_output();
                    unit.set_response(output);
                    unit.set_complete();
                }
            }
            // Internal durable VM state is intentionally not rendered as a
            // chat item. The adjacent Program/Result events are its visible
            // projection.
            BrainEventKind::RuntimeCommitted { .. }
            | BrainEventKind::EffectRecorded { .. }
            | BrainEventKind::EffectAuditTransition { .. }
            | BrainEventKind::ScheduleChanged { .. }
            | BrainEventKind::ScheduleDue { .. } => {}
        }
    }
}
