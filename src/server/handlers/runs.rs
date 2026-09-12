use super::*;

#[cfg(test)]
pub(super) async fn take_run_admission_pause(
    pause: &std::sync::Mutex<Option<RunAdmissionPause>>,
    brain: &str,
) {
    let selected = {
        let mut pause = pause.lock().unwrap();
        if pause.as_ref().is_some_and(|(target, _, _)| target == brain) {
            pause.take()
        } else {
            None
        }
    };
    if let Some((_, reached, release)) = selected {
        let _ = reached.send(());
        let _ = release.await;
    }
}

pub(super) async fn dispatch_named_brain_run(
    store: &crate::brain::store::BrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    run: &crate::brain::store::BrainRun,
) -> anyhow::Result<Option<crate::brain::store::BrainEvent>> {
    use crate::brain::store::{BrainEventKind, BrainRunStatus};

    // The WebSocket command worker is the supervisor for this accepted run.
    // If its transport disappears while the callback is suspended, dropping
    // this future must publish a durable terminal outcome before releasing the
    // Brain lane. The callback response receiver is dropped with the future,
    // fencing any late frontend completion from publication.
    struct DisconnectTerminalizer {
        store: crate::brain::store::BrainStore,
        brain: String,
        run_id: crate::brain::store::RunId,
        request_seq: u64,
        armed: bool,
    }
    impl Drop for DisconnectTerminalizer {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            let detail = "initiating Brain connection disconnected".to_string();
            if let Err(error) = self.store.terminalize_run_with_result_if_active(
                &self.brain,
                "daemon",
                self.run_id,
                self.request_seq,
                BrainRunStatus::Failed,
                detail.clone(),
            ) {
                tracing::error!(brain = %self.brain, run_id = %self.run_id.0, %error,
                    "failed to publish disconnect terminalization; scheduling durable retry");
                self.store.schedule_disconnect_terminalization_retry(
                    self.brain.clone(),
                    "daemon".into(),
                    self.run_id,
                    self.request_seq,
                    BrainRunStatus::Failed,
                    detail,
                );
            }
        }
    }
    let mut terminalizer = DisconnectTerminalizer {
        store: store.clone(),
        brain: name.to_string(),
        run_id: run.run_id,
        request_seq: run.request_seq,
        armed: true,
    };

    let snapshot = store.snapshot(name)?;
    let request = match snapshot
        .events
        .iter()
        .find(|event| event.seq == run.request_seq)
        .cloned()
    {
        Some(request) => request,
        None => {
            let detail = format!("Brain run {} request event is missing", run.run_id.0);
            let result = push_named_brain_run_result(
                store,
                name,
                run.run_id,
                run.request_seq,
                Err(anyhow::anyhow!(detail.clone())),
                Vec::new(),
                None,
            )?;
            store.transition_run(
                name,
                "daemon",
                run.run_id,
                BrainRunStatus::Failed,
                Some(detail),
            )?;
            return Ok(Some(result));
        }
    };
    let projects_memory = matches!(&request.kind, BrainEventKind::Prompt { .. });
    let execution = match request.kind {
        BrainEventKind::Program { language, source } => {
            match dispatch_named_brain_program(
                store,
                runners,
                name,
                run.run_id,
                request.seq,
                language,
                &source,
                crate::server::RunnerProgramInteraction::Interactive,
                None,
            )
            .await
            {
                Ok(result) => Ok((result, None::<crate::server::RunnerTurnCommitAck>)),
                Err(error) => Err(error),
            }
        }
        BrainEventKind::ScheduleDue { due } if due.run.run_id == run.run_id => {
            match dispatch_named_brain_program(
                store,
                runners,
                name,
                run.run_id,
                request.seq,
                due.language,
                &due.source,
                crate::server::RunnerProgramInteraction::Noninteractive,
                Some(due.grant_ceiling),
            )
            .await
            {
                Ok(result) => Ok((result, None::<crate::server::RunnerTurnCommitAck>)),
                Err(error) => Err(error),
            }
        }
        BrainEventKind::Prompt { text } | BrainEventKind::SpeculativePrompt { text } => {
            match snapshot
                .attachments
                .iter()
                .find(|attachment| attachment.attachment_id == run.initiating_attachment_id)
            {
                Some(requester) => {
                    dispatch_named_brain_turn(
                        store,
                        runners,
                        name,
                        run.run_id,
                        request.seq,
                        &text,
                        requester,
                    )
                    .await
                }
                None => Err(anyhow::anyhow!(
                    "Brain run {} initiating attachment is missing",
                    run.run_id.0
                )),
            }
        }
        _ => Err(anyhow::anyhow!(
            "Brain run {} request event is not executable",
            run.run_id.0
        )),
    };

    // Cancellation is authoritative once the initiating driver and exact
    // runner have acknowledged it. A callback that completes after that point
    // must not publish a stale result or overwrite the cancelled state.
    let published = store.inspect_run(name, run.run_id)?;
    if published.status == BrainRunStatus::Cancelled {
        return Ok(None);
    }
    if execution.is_err() && published.status == BrainRunStatus::Failed {
        return Ok(store
            .snapshot(name)?
            .events
            .into_iter()
            .rev()
            .find(|event| {
                event.run_id == Some(run.run_id)
                    && matches!(event.kind, BrainEventKind::Result { .. })
            }));
    }

    let outcome = match execution {
        Ok((result, commit_ack)) => {
            if published.status != BrainRunStatus::Completed {
                store.transition_run(
                    name,
                    "daemon",
                    run.run_id,
                    BrainRunStatus::Completed,
                    None,
                )?;
            }
            if projects_memory {
                if let Err(error) =
                    project_committed_named_brain_memory(store, runners, name, run).await
                {
                    tracing::warn!(
                        brain = name,
                        run_id = %run.run_id.0,
                        %error,
                        "could not project committed Brain turn into memory"
                    );
                }
            }
            if let Some(commit_ack) = commit_ack {
                if let Err(error) = commit_ack.acknowledge(BrainRunStatus::Completed, "") {
                    tracing::warn!(brain = name, run_id = %run.run_id.0, %error, "could not acknowledge committed Brain turn");
                }
            }
            Ok(Some(result))
        }
        Err(error) => {
            let detail = error.to_string();
            if detail == "named Brain run cancelled" {
                // Explicit cancellation has a durable reservation and owns a
                // Cancelled outcome. An ordinary connection teardown only
                // aborts the daemon wait; keep this guard armed so it publishes
                // the disconnect Result+Failed batch.
                terminalizer.armed = !store.run_cancellation_reserved(name, run.run_id)?;
                store.prune_run_publication(name, run.run_id)?;
                return Ok(None);
            }
            let result = push_named_brain_run_result(
                store,
                name,
                run.run_id,
                request.seq,
                Err(anyhow::anyhow!(detail.clone())),
                Vec::new(),
                None,
            )?;
            let status = BrainRunStatus::Failed;
            match store.transition_run(name, "daemon", run.run_id, status, Some(detail)) {
                Ok(_) => {}
                Err(_)
                    if status == BrainRunStatus::Cancelled
                        && store.inspect_run(name, run.run_id)?.status
                            == BrainRunStatus::Cancelled => {}
                Err(error) => return Err(error),
            }
            Ok(Some(result))
        }
    };
    terminalizer.armed = false;
    outcome
}

pub(super) async fn project_committed_named_brain_memory(
    store: &crate::brain::store::BrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    run: &crate::brain::store::BrainRun,
) -> anyhow::Result<usize> {
    let snapshot = store.snapshot(name)?;
    let committed_run = snapshot
        .runs
        .iter()
        .find(|candidate| candidate.run_id == run.run_id)
        .ok_or_else(|| anyhow::anyhow!("committed Brain run disappeared before projection"))?;
    let (prompt, rendered) = committed_named_brain_memory_pair(&snapshot, committed_run)?;
    let lease = snapshot
        .runner_lease
        .as_ref()
        .filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::store::unix_millis()
        })
        .ok_or_else(|| anyhow::anyhow!("committed Brain turn has no live environment runner"))?;
    runners
        .project_memory(
            name,
            lease.lease_id,
            snapshot.brain_id,
            committed_run.run_id,
            committed_run.request_seq,
            prompt,
            rendered,
        )
        .await
}

pub(super) async fn dispatch_named_brain_program(
    store: &crate::brain::store::BrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    run_id: crate::brain::store::RunId,
    request_seq: u64,
    language: crate::brain::store::ProgramLanguage,
    source: &str,
    interaction: crate::server::RunnerProgramInteraction,
    grant_ceiling: Option<crate::vm::EffectSet>,
) -> anyhow::Result<crate::brain::store::BrainEvent> {
    let snapshot = store.snapshot(name)?;
    ensure_named_brain_store_environment(store, &snapshot)?;
    let lease_id = snapshot
        .runner_lease
        .as_ref()
        .filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::store::unix_millis()
        })
        .map(|lease| lease.lease_id)
        .ok_or_else(|| anyhow::anyhow!("named Brain '{name}' has no live environment runner"))?;
    let outcome = match runners
        .dispatch_program(
            name,
            lease_id,
            run_id,
            request_seq,
            language,
            source.to_string(),
            interaction,
            grant_ceiling,
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<crate::server::RunnerProgramError>() {
                let publication = store.acquire_run_publication(name, run_id).await?;
                if publication.cancel_requested()
                    || store.run_cancellation_reserved(name, run_id)?
                {
                    drop(publication);
                    anyhow::bail!("named Brain run cancelled");
                }
                validate_runner_effect_journal(&failure.effect_journal)?;
                push_named_brain_run_result(
                    store,
                    name,
                    run_id,
                    request_seq,
                    Err(anyhow::anyhow!(error.to_string())),
                    Vec::new(),
                    None,
                )?;
                store.transition_run(
                    name,
                    "daemon",
                    run_id,
                    crate::brain::store::BrainRunStatus::Failed,
                    Some(error.to_string()),
                )?;
                drop(publication);
                store.prune_run_publication(name, run_id)?;
            }
            return Err(error);
        }
    };
    let publication = store.acquire_run_publication(name, run_id).await?;
    if publication.cancel_requested() || store.run_cancellation_reserved(name, run_id)? {
        drop(publication);
        anyhow::bail!("named Brain run cancelled");
    }
    validate_runner_effect_journal(&outcome.effect_journal)?;
    store.commit_runner_runtime_for_run(
        name,
        run_id,
        request_seq,
        outcome.runtime_revision,
        outcome.checkpoint,
    )?;
    let result = push_named_brain_run_result(
        store,
        name,
        run_id,
        request_seq,
        Ok(outcome.output),
        Vec::new(),
        None,
    )?;
    store.transition_run(
        name,
        "daemon",
        run_id,
        crate::brain::store::BrainRunStatus::Completed,
        None,
    )?;
    drop(publication);
    store.prune_run_publication(name, run_id)?;
    Ok(result)
}

pub(super) async fn dispatch_named_brain_turn(
    store: &crate::brain::store::BrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    run_id: crate::brain::store::RunId,
    request_seq: u64,
    prompt: &str,
    requester: &crate::brain::store::BrainAttachment,
) -> anyhow::Result<(
    crate::brain::store::BrainEvent,
    Option<crate::server::RunnerTurnCommitAck>,
)> {
    let snapshot = store.snapshot(name)?;
    ensure_named_brain_store_environment(store, &snapshot)?;
    let lease = snapshot
        .runner_lease
        .as_ref()
        .filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::store::unix_millis()
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("named Brain '{name}' has no live environment runner"))?;
    let lease_id = lease.lease_id;
    // Queued runs survive daemon restart independently of the transport which
    // initiated them. Preserve a live connection generation when present, but
    // allow a restored connectionless turn to execute; its reverse approval
    // control will fail closed if it later requires an addressed decision.
    let approval_connection_id = requester.connection_id;
    let approval_audience = crate::brain::store::BrainApprovalAudience {
        brain_id: snapshot.brain_id,
        brain: name.to_string(),
        attachment_id: requester.attachment_id,
        subject: requester.subject.clone(),
        role: requester.role,
        environment_generation: snapshot.environment.generation,
    };
    let outcome = match runners
        .dispatch_turn(
            name,
            lease_id,
            run_id,
            request_seq,
            prompt.to_string(),
            named_brain_provider_messages_at(&snapshot, request_seq),
            approval_audience.clone(),
            approval_connection_id,
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<crate::server::RunnerTurnError>() {
                let publication = store.acquire_run_publication(name, run_id).await?;
                if publication.cancel_requested()
                    || store.run_cancellation_reserved(name, run_id)?
                {
                    drop(publication);
                    anyhow::bail!("named Brain run cancelled");
                }
                persist_named_brain_turn_events(
                    store,
                    name,
                    Some(run_id),
                    request_seq,
                    &lease.subject,
                    &approval_audience,
                    failure.turn_events.clone(),
                )?;
                validate_runner_effect_journal(&failure.effect_journal)?;
                push_named_brain_run_result(
                    store,
                    name,
                    run_id,
                    request_seq,
                    Err(anyhow::anyhow!(error.to_string())),
                    Vec::new(),
                    None,
                )?;
                store.transition_run(
                    name,
                    "daemon",
                    run_id,
                    crate::brain::store::BrainRunStatus::Failed,
                    Some(error.to_string()),
                )?;
                drop(publication);
                store.prune_run_publication(name, run_id)?;
            }
            return Err(error);
        }
    };
    let publication = store.acquire_run_publication(name, run_id).await?;
    if publication.cancel_requested() || store.run_cancellation_reserved(name, run_id)? {
        drop(publication);
        anyhow::bail!("named Brain run cancelled");
    }
    let commit_ack = outcome.commit_ack.clone();
    persist_named_brain_turn_events(
        store,
        name,
        Some(run_id),
        request_seq,
        &lease.subject,
        &approval_audience,
        outcome.turn_events,
    )?;
    validate_runner_effect_journal(&outcome.effect_journal)?;
    let program = store.push_for_run(
        name,
        "provider",
        run_id,
        crate::brain::store::BrainEventKind::Program {
            language: outcome.language,
            source: outcome.source,
        },
    )?;
    store.commit_runner_runtime_for_run(
        name,
        run_id,
        program.seq,
        outcome.runtime_revision,
        outcome.checkpoint,
    )?;
    let result = push_named_brain_run_result(
        store,
        name,
        run_id,
        program.seq,
        Ok(outcome.output),
        outcome.continuation_messages,
        outcome.invocation_metadata,
    )?;
    store.transition_run(
        name,
        "daemon",
        run_id,
        crate::brain::store::BrainRunStatus::Completed,
        None,
    )?;
    drop(publication);
    store.prune_run_publication(name, run_id)?;
    Ok((result, commit_ack))
}

pub(super) async fn watch_named_brain(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(connection): Query<WatchNamedBrainQuery>,
    ws: axum::extract::WebSocketUpgrade,
) -> Result<Response, Response> {
    let attachment_id = crate::brain::store::AttachmentId(connection.attachment_id);
    let connection_id = crate::brain::store::ConnectionId(connection.connection_id);
    let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
    authorize_pending_remote_attachment(
        &lifecycle,
        server.brain_credentials(),
        &headers,
        &name,
        attachment_id,
        connection_id,
    )?;
    let watch = lifecycle
        .watch(&name, attachment_id, connection_id)
        .map_err(|error| AppError(error).into_response())?;
    let snapshot = watch.snapshot;
    let mut events = watch.events;
    let command_server = server.clone();
    Ok(ws
        .on_upgrade(move |mut socket| async move {
            use axum::extract::ws::Message as WsMessage;
            use crate::ipc::brain_codec::{
                BrainRemoteCommand, BrainRemoteEnvelope, BrainRemoteReply,
            };

            let (command_tx, mut command_rx) =
                tokio::sync::mpsc::unbounded_channel::<BrainRemoteCommand>();
            let (approval_tx, mut approval_rx) =
                tokio::sync::mpsc::unbounded_channel::<BrainRemoteCommand>();
            let (reply_tx, mut reply_rx) =
                tokio::sync::mpsc::unbounded_channel::<BrainRemoteReply>();
            let worker_name = name.clone();
            let worker_headers = headers.clone();
            let approval_server = server.clone();
            let approval_name = name.clone();
            let approval_headers = headers.clone();
            let approval_reply_tx = reply_tx.clone();
            let worker = tokio::spawn(async move {
                while let Some(command) = command_rx.recv().await {
                    let reply = execute_remote_brain_command(
                        &command_server,
                        &worker_headers,
                        &worker_name,
                        attachment_id,
                        connection_id,
                        command,
                    )
                    .await;
                    let detached = matches!(reply, BrainRemoteReply::Detached { .. });
                    if reply_tx.send(reply).is_err() || detached {
                        break;
                    }
                }
            });
            // A runner may suspend an executable command while it awaits an
            // approval from this same socket. Keep approval decisions ordered
            // with each other, but do not queue them behind that suspended
            // command (or behind the Brain turn lane it holds).
            let approval_worker = tokio::spawn(async move {
                while let Some(command) = approval_rx.recv().await {
                    let reply = execute_remote_brain_command(
                        &approval_server,
                        &approval_headers,
                        &approval_name,
                        attachment_id,
                        connection_id,
                        command,
                    )
                    .await;
                    if approval_reply_tx.send(reply).is_err() {
                        break;
                    }
                }
            });

            let initial = BrainRemoteEnvelope::Projection(
                crate::brain::store::BrainWireMessage::Snapshot { brain: snapshot },
            );
            if let Ok(encoded) = crate::ipc::brain_codec::encode_brain_remote_envelope(&initial) {
                if socket
                    .send(WsMessage::Binary(encoded.into()))
                    .await
                    .is_err()
                {
                    let _ = lifecycle.detach(&name, attachment_id, connection_id);
                    return;
                }
            }
            let mut authority_tick =
                tokio::time::interval(std::time::Duration::from_secs(5));
            authority_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut replies_open = true;
            let mut detach_request_id = None;
            let mut pending_detach_projection = None;
            loop {
                tokio::select! {
                    incoming = socket.recv() => match incoming {
                        Some(Ok(WsMessage::Ping(payload))) => {
                            if socket.send(WsMessage::Pong(payload)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(WsMessage::Binary(bytes))) => {
                            match crate::ipc::brain_codec::decode_brain_remote_envelope(&bytes) {
                                Ok(BrainRemoteEnvelope::Command(command)) => {
                                    if matches!(
                                        &command.kind,
                                        crate::ipc::brain_codec::BrainRemoteCommandKind::Detach
                                    ) {
                                        detach_request_id = Some(command.request_id);
                                    }
                                    let is_approval = matches!(
                                        &command.kind,
                                        crate::ipc::brain_codec::BrainRemoteCommandKind::Submit(
                                            crate::brain::store::BrainEventKind::ApprovalDecided { .. }
                                        )
                                    );
                                    let sent = if is_approval {
                                        approval_tx.send(command)
                                    } else {
                                        command_tx.send(command)
                                    };
                                    if sent.is_err() {
                                        break;
                                    }
                                }
                                _ => break,
                            }
                        }
                        Some(Ok(WsMessage::Pong(_))) => continue,
                        Some(Ok(_)) => break,
                    },
                    reply = reply_rx.recv(), if replies_open => {
                        let Some(reply) = reply else {
                            replies_open = false;
                            continue;
                        };
                        let reply_request_id = reply.request_id();
                        let detached = matches!(&reply, BrainRemoteReply::Detached { .. });
                        let detach_failed = matches!(&reply, BrainRemoteReply::Error { .. })
                            && detach_request_id == Some(reply_request_id);
                        let envelope = BrainRemoteEnvelope::Reply(reply);
                        let Ok(encoded) = crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope) else {
                            break;
                        };
                        #[cfg(test)]
                        if DROP_NEXT_REMOTE_BRAIN_REPLY.swap(
                            false,
                            std::sync::atomic::Ordering::SeqCst,
                        ) {
                            break;
                        }
                        if socket.send(WsMessage::Binary(encoded.into())).await.is_err() {
                            break;
                        }
                        if detached {
                            detach_request_id = None;
                            if let Some(wire) = pending_detach_projection.take() {
                                let envelope = BrainRemoteEnvelope::Projection(wire);
                                let Ok(encoded) = crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope) else {
                                    break;
                                };
                                let _ = socket.send(WsMessage::Binary(encoded.into())).await;
                                break;
                            }
                        } else if detach_failed {
                            detach_request_id = None;
                        }
                    }
                    event = events.recv() => {
                        let (wire, closes_attachment) = match event {
                        Ok(event) => {
                            let closes_attachment = matches!(
                                &event.kind,
                                crate::brain::store::BrainEventKind::ClientDetached {
                                    attachment_id: detached,
                                    connection_id: disconnected,
                                } if *detached == attachment_id && *disconnected == connection_id
                            );
                            (crate::brain::store::BrainWireMessage::Event { event }, closes_attachment)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let Ok(brain) = lifecycle.snapshot(&name) else {
                                break;
                            };
                            (crate::brain::store::BrainWireMessage::Snapshot { brain }, false)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        };
                        if closes_attachment && detach_request_id.is_some() {
                            pending_detach_projection = Some(wire);
                            continue;
                        }
                        let envelope = BrainRemoteEnvelope::Projection(wire);
                        let Ok(encoded) = crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope) else {
                            break;
                        };
                        if socket.send(WsMessage::Binary(encoded.into())).await.is_err()
                            || closes_attachment
                        {
                            break;
                        }
                    }
                    _ = authority_tick.tick() => {
                        if authorize_named_brain(
                            &server,
                            &headers,
                            &name,
                            crate::brain::credential::BrainCredentialScope::BrainRead,
                        ).is_err()
                            || lifecycle.connection(
                                &name,
                                attachment_id,
                                connection_id,
                            ).is_err()
                        {
                            break;
                        }
                    }
                }
            }
            drop(command_tx);
            drop(approval_tx);
            // This socket owns only this opaque connection generation. Detach
            // it before waiting on command futures: an ordinary command may be
            // holding the Brain turn lane while suspended on an approval whose
            // only audience was this connection. `detach` first validates the
            // exact attachment/connection pair, fails those addressed
            // approvals closed, and deliberately leaves the durable runner
            // lease alone.
            teardown_remote_brain_connection(
                &lifecycle,
                &name,
                attachment_id,
                connection_id,
                worker,
                approval_worker,
            )
            .await;
        })
        .into_response())
}
