// Durable named-Brain handlers for EventLoop.
//
// This file is included verbatim into event_loop.rs. There is intentionally
// no client-local speculative Brain session: background work must enter the
// canonical service as a correlated BrainRun.

impl EventLoop {
    /// Update the vocabulary panel from partially typed input. This is local
    /// presentation only and never starts provider or workspace work.
    async fn handle_typing_started(&self, partial: String) {
        let mut seen = std::collections::HashSet::new();
        let words = partial
            .split(|character: char| {
                !character.is_alphabetic() && character != '-' && character != '\''
            })
            .filter(|word| word.len() >= 3)
            .map(str::to_lowercase)
            .filter(|word| seen.insert(word.clone()))
            .collect();
        self.tui_renderer.lock().await.set_typing_words(words);
    }
}

// ── Daemon brain command handlers ─────────────────────────────────────────────

impl EventLoop {
    /// Register the frontend's home Brain in the daemon's durable named store.
    /// Register the frontend's home namespace and maintain its expiring
    /// environment-runner lease. A failed renewal immediately removes the
    /// runner claim from the status bar; retry never reuses an expired ID.
    async fn register_home_brain(
        &self,
    ) -> Result<Option<std::result::Result<crate::brain::shared::RunnerLeaseId, String>>> {
        let Some(ipc) = self.ipc_client.as_ref().cloned() else {
            return Ok(None);
        };
        let snapshot = ipc.brain_snapshot(&self.session_label).await?;
        let initial = ipc
            .brain_acquire_runner(
                &self.session_label,
                &self.runner_subject,
                &snapshot.environment,
                None,
                30_000,
            )
            .await;
        let initial_registration = match (&initial, self.ipc_client.as_ref()) {
            (Ok(lease), Some(ipc)) => match ipc
                .register_brain_runner(&self.session_label, lease.lease_id, self.event_tx.clone())
                .await
            {
                Ok(bootstrap) => self
                    .program_runtime
                    .hydrate_reducible_state_if_newer(
                        bootstrap.checkpoint,
                        bootstrap.runtime_revision,
                    )
                    .await
                    .map(|_| lease.lease_id)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            (Ok(_), None) => Err("Cap'n Proto daemon connection unavailable".into()),
            (Err(error), _) => Err(error.to_string()),
        };
        let initial_had_lease = initial.is_ok();
        let lease_id = initial.ok().map(|lease| lease.lease_id);
        self.start_runner_lease_renewal(
            ipc,
            snapshot.name,
            self.runner_subject.clone(),
            snapshot.environment,
            lease_id,
            initial_had_lease,
        );
        Ok(Some(initial_registration))
    }

    fn start_runner_lease_renewal(
        &self,
        ipc: crate::ipc::IpcClient,
        brain: String,
        subject: String,
        environment: crate::brain::shared::BrainEnvironment,
        mut lease_id: Option<crate::brain::shared::RunnerLeaseId>,
        initial_had_lease: bool,
    ) {
        let event_tx = self.event_tx.clone();
        let epoch = self
            .runner_renewal_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let renewal_epoch = self.runner_renewal_epoch.clone();
        tokio::task::spawn_local(async move {
            let mut had_lease = initial_had_lease;
            loop {
                tokio::select! {
                    _ = event_tx.closed() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                }
                if renewal_epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
                    break;
                }
                match ipc
                    .brain_acquire_runner(&brain, &subject, &environment, lease_id, 30_000)
                    .await
                {
                    Ok(lease) => {
                        lease_id = Some(lease.lease_id);
                        // Re-register on every renewal. Replacing the broker
                        // sender closes the previous callback bridge, so this
                        // also repairs a dropped Cap'n Proto callback without
                        // pretending that lease ownership alone means the
                        // runner is reachable.
                        let _ = event_tx.send(ReplEvent::RunnerLeaseStatus {
                            brain: brain.clone(),
                            epoch,
                            lease_id: Some(lease.lease_id),
                            detail: if had_lease {
                                "lease renewed".into()
                            } else {
                                "lease reacquired".into()
                            },
                        });
                        had_lease = true;
                    }
                    Err(error) => {
                        // A deliberate handoff is a terminal ownership
                        // transition, not an ordinary expiry. Preserve the
                        // old identity until the durable log proves which
                        // case occurred; otherwise a disconnected source
                        // could reclaim the Brain after the target exits.
                        let inspected_handoff = match lease_id {
                            Some(previous) => ipc
                                .brain_snapshot(&brain)
                                .await
                                .ok()
                                .map(|brain| brain.runner_lease_was_handed_off(previous)),
                            None => Some(false),
                        };
                        let handed_off = inspected_handoff == Some(true);
                        if had_lease {
                            let _ = event_tx.send(ReplEvent::RunnerLeaseStatus {
                                brain: brain.clone(),
                                epoch,
                                lease_id: None,
                                detail: if handed_off {
                                    "runner lease handed off to another frontend".into()
                                } else {
                                    error.to_string()
                                },
                            });
                        }
                        had_lease = false;
                        if handed_off {
                            break;
                        }
                        // Only a successfully inspected, non-handoff expiry
                        // may discard the old identity and acquire a new one.
                        if inspected_handoff == Some(false) {
                            lease_id = None;
                        }
                    }
                }
            }
        });
    }

    /// Attach the home console as a driver to the same durable Brain whose
    /// environment-runner lease it owns. Runner authority and conversation
    /// participation remain distinct, but both now observe one event stream.
    async fn attach_home_brain(&mut self) -> Result<()> {
        let base = self
            .daemon_base_url
            .as_deref()
            .context("local daemon is unavailable")?;
        let target = crate::brain::remote::RemoteBrainTarget::local(&self.session_label, base)?;
        let ipc = self
            .ipc_client
            .as_ref()
            .context("Cap'n Proto daemon connection unavailable")?
            .clone();
        let mut client = crate::brain::remote::AttachedBrainClient::local(target, ipc);
        client
            .attach_persistent(
                &self.participant_subject,
                crate::brain::shared::AttachmentRole::Driver,
                &self.session_label,
            )
            .await?;
        let mut incoming = client.watch().await?;
        let snapshot = match incoming.recv().await {
            Some(crate::brain::shared::BrainWireMessage::Snapshot { brain }) => brain,
            Some(crate::brain::shared::BrainWireMessage::Event { .. }) => {
                anyhow::bail!("home Brain event stream did not begin with a snapshot")
            }
            None => anyhow::bail!("home Brain event stream closed before its snapshot"),
        };
        client.target.machine = snapshot.environment.machine.clone();
        let target_name = client.target.display_name();
        self.home_brain = Some(client);
        self.render_remote_brain_message(crate::brain::shared::BrainWireMessage::Snapshot {
            brain: snapshot.clone(),
        })
        .await?;
        if let Some(client) = self.home_brain.as_mut() {
            client.acknowledge(snapshot.revision).await?;
        }

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

    /// Best-effort graceful teardown for the durable presence this frontend
    /// established at startup. The daemon still expires crashed runners, but
    /// an ordinary `/quit` must make callback availability exact immediately.
    async fn release_home_brain_presence(&mut self) {
        self.runner_renewal_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let (Some(ipc), Some(brain), Some(lease_id)) = (
            self.ipc_client.as_ref(),
            self.runner_brain.take(),
            self.home_runner_lease_id.take(),
        ) {
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                ipc.brain_release_runner(&brain, lease_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("home Brain runner release failed: {error}"),
                Err(_) => tracing::warn!("home Brain runner release timed out"),
            }
        }
        self.home_runner_lease_active = false;

        if let Some(client) = self.active_remote_brain.take() {
            if tokio::time::timeout(std::time::Duration::from_secs(2), client.disconnect())
                .await
                .is_err()
            {
                tracing::warn!("remote Brain detach timed out during shutdown");
            }
        }
        if let Some(client) = self.home_brain.take() {
            if tokio::time::timeout(std::time::Duration::from_secs(2), client.disconnect())
                .await
                .is_err()
            {
                tracing::warn!("home Brain detach timed out during shutdown");
            }
        }
    }

    /// Handle `/brains` — list the daemon's authoritative named Brains.
    /// Legacy background workers are runs/tasks, not another Brain namespace.
    async fn handle_brains_list(&mut self) -> Result<()> {
        let Some(base) = self.daemon_base_url.as_deref() else {
            self.output_manager.write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };

        #[derive(serde::Deserialize)]
        struct NamedBrainSummary {
            name: String,
            environment: crate::brain::shared::BrainEnvironment,
            event_revision: u64,
            retained_programs: usize,
            runner: Option<crate::brain::shared::BrainRunnerLease>,
        }

        let brains = match reqwest::Client::new()
            .get(format!("{base}/v1/brains/named"))
            .send()
            .await
        {
            Ok(response) => match response.error_for_status() {
                Ok(response) => response
                    .json::<Vec<NamedBrainSummary>>()
                    .await
                    .map_err(anyhow::Error::from),
                Err(error) => Err(anyhow::Error::from(error)),
            },
            Err(error) => Err(anyhow::Error::from(error)),
        };

        match brains {
            Ok(brains) if brains.is_empty() => {
                self.output_manager.write_info("No named Brains.");
            }
            Ok(brains) => {
                let current_name = self
                    .active_remote_brain
                    .as_ref()
                    .map(|client| client.target.brain.as_str())
                    .unwrap_or(self.session_label.as_str());
                let mut lines =
                    vec!["Named Brains (event revision · retained program stack):".to_string()];
                for b in &brains {
                    let current = if b.name == current_name {
                        if self.active_remote_brain.is_some() {
                            " · current driver"
                        } else {
                            " · current home"
                        }
                    } else {
                        ""
                    };
                    let runner = b
                        .runner
                        .as_ref()
                        .map(|lease| format!(" · runner {}", lease.subject))
                        .unwrap_or_else(|| " · runner offline".into());
                    lines.push(format!(
                        "  {:55}  event {:<5}  {:<3} retained programs  {}{}{}",
                        format!("{}@{}", b.name, b.environment.machine),
                        b.event_revision,
                        b.retained_programs,
                        b.environment.workspace.display(),
                        runner,
                        current,
                    ));
                }
                self.output_manager.write_info(lines.join("\n"));
            }
            Err(e) => {
                self.output_manager
                    .write_info(format!("⚠️  Failed to list brains: {}", e));
            }
        }

        self.render_tui().await
    }

    async fn handle_brain_archive(&mut self, name: String) -> Result<()> {
        let active_name = self
            .active_remote_brain
            .as_ref()
            .map(|client| client.target.brain.as_str())
            .unwrap_or(self.session_label.as_str());
        if name == active_name {
            self.output_manager
                .write_info("detach from or switch away from a Brain before archiving it");
            return self.render_tui().await;
        }
        let Some(base) = self.daemon_base_url.as_deref() else {
            self.output_manager.write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };
        let target = crate::brain::remote::RemoteBrainTarget::local(&name, base)?;
        let password = crate::config::load_config()
            .map(|config| config.server.brain_password)
            .unwrap_or_default();
        let client = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        match client.archive(&self.participant_subject).await {
            Ok(archived_to) => {
                let destination = archived_to.as_deref().unwrap_or("in-memory archive");
                self.output_manager
                    .write_info(format!("archived Brain {name} → {destination}"));
            }
            Err(error) => self
                .output_manager
                .write_info(format!("could not archive Brain {name}: {error}")),
        }
        self.render_tui().await
    }

    fn selected_handoff(
        snapshot: &crate::brain::shared::BrainSnapshot,
        requested: Option<&str>,
    ) -> Result<crate::brain::shared::BrainRunnerHandoff> {
        let handoff = snapshot
            .runner_handoff
            .clone()
            .context("the selected Brain has no pending runner handoff")?;
        if let Some(requested) = requested {
            let actual = handoff.handoff_id.0.to_string();
            if !actual.starts_with(requested) {
                anyhow::bail!("pending runner handoff ID does not match '{requested}'");
            }
        }
        Ok(handoff)
    }

    async fn remote_handoff_control_client(
        &self,
        target: crate::brain::remote::RemoteBrainTarget,
    ) -> Result<(
        crate::brain::remote::RemoteBrainClient,
        tokio::sync::mpsc::UnboundedReceiver<crate::brain::shared::BrainWireMessage>,
    )> {
        let password = crate::config::load_config()
            .map(|config| config.server.brain_password)
            .unwrap_or_default();
        let mut client = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        client
            .authorize_runner_handoff_control(
                &self.participant_subject,
                crate::brain::shared::AttachmentRole::Driver,
            )
            .await?;
        client
            .attach(
                &self.participant_subject,
                crate::brain::shared::AttachmentRole::Driver,
                None,
            )
            .await?;
        let events = client.watch().await?;
        Ok((client, events))
    }

    async fn handle_brain_handoff(&mut self, target_subject: String) -> Result<()> {
        let selected = self
            .selected_brain()
            .context("attach to the Brain whose runner should be transferred")?;
        let target = selected.target.clone();
        let snapshot = selected.snapshot().await?;
        let source = snapshot
            .runner_lease
            .as_ref()
            .context("the selected Brain has no live runner to hand off")?;
        let handoff = if self.active_remote_brain.is_some() {
            let (client, _events) = self.remote_handoff_control_client(target).await?;
            let result = client
                .request_runner_handoff(
                    &target_subject,
                    source.lease_id,
                    snapshot.environment.generation,
                    30_000,
                )
                .await;
            let disconnect = client.disconnect().await;
            match (result, disconnect) {
                (Ok(handoff), _) => handoff,
                (Err(error), _) => return Err(error),
            }
        } else {
            self.ipc_client
                .as_ref()
                .context("Cap'n Proto daemon connection unavailable")?
                .brain_request_runner_handoff(
                    &snapshot.name,
                    &self.participant_subject,
                    &target_subject,
                    source.lease_id,
                    &snapshot.environment,
                    30_000,
                )
                .await?
        };
        self.output_manager.write_info(format!(
            "runner handoff {} requested for {}",
            &handoff.handoff_id.0.to_string()[..8],
            handoff.target_subject
        ));
        self.render_tui().await
    }

    async fn handle_brain_handoff_cancel(&mut self, requested: Option<String>) -> Result<()> {
        let selected = self
            .selected_brain()
            .context("attach to the Brain whose handoff should be cancelled")?;
        let target = selected.target.clone();
        let snapshot = selected.snapshot().await?;
        let handoff = Self::selected_handoff(&snapshot, requested.as_deref())?;
        if self.active_remote_brain.is_some() {
            let (client, _events) = self.remote_handoff_control_client(target).await?;
            let result = client.cancel_runner_handoff(handoff.handoff_id).await;
            let disconnect = client.disconnect().await;
            match (result, disconnect) {
                (Ok(()), _) => {}
                (Err(error), _) => return Err(error),
            }
        } else {
            self.ipc_client
                .as_ref()
                .context("Cap'n Proto daemon connection unavailable")?
                .brain_cancel_runner_handoff(
                    &snapshot.name,
                    handoff.handoff_id,
                    &self.participant_subject,
                )
                .await?;
        }
        self.output_manager.write_info(format!(
            "runner handoff {} cancelled",
            &handoff.handoff_id.0.to_string()[..8]
        ));
        self.render_tui().await
    }

    async fn restore_runner_after_failed_handoff(
        &mut self,
        ipc: &crate::ipc::IpcClient,
        previous: Option<(String, crate::brain::shared::BrainEnvironment)>,
    ) -> Result<()> {
        let Some((brain, environment)) = previous else {
            return Ok(());
        };
        let lease = ipc
            .brain_acquire_runner(&brain, &self.runner_subject, &environment, None, 30_000)
            .await
            .with_context(|| format!("restore runner lease for {brain}"))?;
        if let Err(error) = ipc
            .register_brain_runner(&brain, lease.lease_id, self.event_tx.clone())
            .await
        {
            let _ = ipc.brain_release_runner(&brain, lease.lease_id).await;
            return Err(error.context(format!("restore runner callback for {brain}")));
        }
        self.runner_brain = Some(brain.clone());
        self.home_runner_lease_id = Some(lease.lease_id);
        self.home_runner_lease_active = true;
        self.start_runner_lease_renewal(
            ipc.clone(),
            brain,
            self.runner_subject.clone(),
            environment,
            Some(lease.lease_id),
            true,
        );
        Ok(())
    }

    async fn fail_handoff_and_restore_runner(
        &mut self,
        ipc: &crate::ipc::IpcClient,
        previous: Option<(String, crate::brain::shared::BrainEnvironment)>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        match self
            .restore_runner_after_failed_handoff(ipc, previous)
            .await
        {
            Ok(()) => error,
            Err(restore_error) => error.context(format!(
                "the previous runner could not be restored: {restore_error:#}"
            )),
        }
    }

    async fn handle_brain_handoff_accept(&mut self, requested: Option<String>) -> Result<()> {
        let selected = self
            .selected_brain()
            .context("attach to the Brain whose handoff should be accepted")?;
        let selected_target = selected.target.clone();
        let snapshot = selected.snapshot().await?;
        let handoff = Self::selected_handoff(&snapshot, requested.as_deref())?;
        anyhow::ensure!(
            handoff.target_subject == self.runner_subject,
            "runner handoff is addressed to {}, not this frontend ({})",
            handoff.target_subject,
            self.runner_subject
        );

        if self.active_remote_brain.is_some() {
            let base = self
                .daemon_base_url
                .as_deref()
                .context("local daemon is unavailable")?;
            let local = crate::brain::remote::RemoteBrainTarget::local(&snapshot.name, base)?;
            anyhow::ensure!(
                selected_target.address == local.address,
                "runner handoff acceptance must run on the Brain environment host"
            );
        }
        let ipc = self
            .ipc_client
            .as_ref()
            .context("Cap'n Proto daemon connection unavailable")?
            .clone();

        anyhow::ensure!(
            self.runner_brain.as_deref() != Some(snapshot.name.as_str()),
            "this frontend already serves {}",
            snapshot.name
        );

        let previous_runner = match (self.runner_brain.as_ref(), self.home_runner_lease_id) {
            (Some(brain), Some(_)) => Some((
                brain.clone(),
                ipc.brain_snapshot(brain)
                    .await
                    .with_context(|| format!("inspect current runner Brain {brain}"))?
                    .environment,
            )),
            _ => None,
        };

        self.runner_renewal_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let (Some(current_brain), Some(current_lease)) =
            (self.runner_brain.as_ref(), self.home_runner_lease_id)
        {
            ipc.brain_release_runner(&current_brain, current_lease)
                .await
                .with_context(|| format!("release runner lease for {current_brain}"))?;
        }
        self.runner_brain = None;
        self.home_runner_lease_id = None;
        self.home_runner_lease_active = false;

        let lease = match ipc
            .brain_accept_runner_handoff(
                &snapshot.name,
                &self.runner_subject,
                handoff.handoff_id,
                &snapshot.environment,
                30_000,
            )
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                return Err(self
                    .fail_handoff_and_restore_runner(&ipc, previous_runner, error)
                    .await);
            }
        };
        let registration = ipc
            .register_brain_runner(&snapshot.name, lease.lease_id, self.event_tx.clone())
            .await;
        let bootstrap = match registration {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                let _ = ipc
                    .brain_release_runner(&snapshot.name, lease.lease_id)
                    .await;
                let error = error.context("register accepted runner callback");
                return Err(self
                    .fail_handoff_and_restore_runner(&ipc, previous_runner, error)
                    .await);
            }
        };
        if let Err(error) = self
            .program_runtime
            .replace_reducible_state(bootstrap.checkpoint, bootstrap.runtime_revision)
            .await
        {
            let _ = ipc
                .brain_release_runner(&snapshot.name, lease.lease_id)
                .await;
            let error = error.context("hydrate accepted Brain runtime");
            return Err(self
                .fail_handoff_and_restore_runner(&ipc, previous_runner, error)
                .await);
        }

        self.runner_brain = Some(snapshot.name.clone());
        self.home_runner_lease_id = Some(lease.lease_id);
        self.home_runner_lease_active = true;
        self.start_runner_lease_renewal(
            ipc,
            snapshot.name.clone(),
            self.runner_subject.clone(),
            snapshot.environment,
            Some(lease.lease_id),
            true,
        );
        self.output_manager.write_info(format!(
            "accepted runner handoff {}; this frontend now serves {}",
            &handoff.handoff_id.0.to_string()[..8],
            snapshot.name
        ));
        self.update_remote_brain_status(true);
        self.render_tui().await
    }
}
