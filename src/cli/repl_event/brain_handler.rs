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
                &self.participant_subject,
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
        let mut lease_id = initial.ok().map(|lease| lease.lease_id);
        let event_tx = self.event_tx.clone();
        let subject = self.participant_subject.clone();
        let environment = snapshot.environment;
        tokio::task::spawn_local(async move {
            let mut had_lease = initial_had_lease;
            loop {
                tokio::select! {
                    _ = event_tx.closed() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                }
                match ipc
                    .brain_acquire_runner(&snapshot.name, &subject, &environment, lease_id, 30_000)
                    .await
                {
                    Ok(lease) => {
                        lease_id = Some(lease.lease_id);
                        // Re-register on every renewal. Replacing the broker
                        // sender closes the previous callback bridge, so this
                        // also repairs a dropped Cap'n Proto callback without
                        // pretending that lease ownership alone means the
                        // runner is reachable.
                        let _ = event_tx.send(ReplEvent::HomeRunnerLeaseStatus {
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
                                .brain_snapshot(&snapshot.name)
                                .await
                                .ok()
                                .map(|brain| brain.runner_lease_was_handed_off(previous)),
                            None => Some(false),
                        };
                        let handed_off = inspected_handoff == Some(true);
                        if had_lease {
                            let _ = event_tx.send(ReplEvent::HomeRunnerLeaseStatus {
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
        Ok(Some(initial_registration))
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
        if let (Some(ipc), Some(lease_id)) =
            (self.ipc_client.as_ref(), self.home_runner_lease_id.take())
        {
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                ipc.brain_release_runner(&self.session_label, lease_id),
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
}
