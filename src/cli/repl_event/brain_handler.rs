// In-process context-gathering and durable Brain session handlers for EventLoop.
//
// This file is included verbatim into src/cli/repl_event/event_loop.rs by:
//
//   include!("brain_handler.rs");
//
// Because `include!` pastes the content at the call site, this file shares
// the event_loop module's namespace: all EventLoop fields, imports from
// event_loop.rs, and crate-level types are directly in scope.
//
// This is NOT a Rust module — it has no `mod` declaration.
//
// ── Summary of contents ─────────────────────────────────────────────────────
//
// impl EventLoop (in-process brain):
//   cancel_active_brain     — cancel + optionally discard gathered context
//   handle_typing_started   — debounce-spawns brain on partial input
//   handle_brain_question   — shows dialog for in-process brain question
//   handle_brain_proposed_action — shows Yes/No dialog for brain action
//
// impl EventLoop (durable Brain sessions):
//   handle_brains_list      — /brains: lists authoritative named Brains
//   handle_brain_archive    — archives an inactive named Brain

// ── In-process brain handlers ─────────────────────────────────────────────────

impl EventLoop {
    /// Cancel the active brain session.
    ///
    /// `clear_context` controls whether the pre-gathered context is discarded:
    /// - `true`  — typing restarted (new partial query); old context is stale, discard it.
    /// - `false` — user submitted; keep context so `handle_user_input` can inject it.
    async fn cancel_active_brain(&self, clear_context: bool) {
        if let Some(session) = self.active_brain.write().await.take() {
            session.cancel();
        }
        if clear_context {
            *self.brain_context.write().await = None;
        }
    }

    /// Handle a `TypingStarted` event: update the word panel.
    /// No AI calls are made while typing — functions only fire on submit.
    async fn handle_typing_started(&self, partial: String) {
        // Extract words from the partial input and show arrows in the panel.
        let mut seen = std::collections::HashSet::new();
        let words: Vec<String> = partial
            .split(|c: char| !c.is_alphabetic() && c != '-' && c != '\'')
            .filter(|w| w.len() >= 3)
            .map(|w| w.to_lowercase())
            .filter(|w| seen.insert(w.clone()))
            .collect();
        let mut tui = self.tui_renderer.lock().await;
        tui.set_typing_words(words);
    }

    /// Handle a `BrainQuestion` event: show a dialog and store the response channel.
    ///
    /// If the user is currently busy (active query in flight), defer the question
    /// until they become idle — the brain waits rather than interrupting.
    async fn handle_brain_question(
        &mut self,
        question: String,
        options: Vec<String>,
        response_tx: tokio::sync::oneshot::Sender<String>,
    ) -> Result<()> {
        tracing::debug!("[EVENT_LOOP] Brain question: {}", question);

        // If a query is in flight, defer the question — don't interrupt the user.
        let is_busy = self.active_query_id.read().await.is_some();
        if is_busy {
            tracing::debug!("[EVENT_LOOP] User busy — deferring brain question");
            // Replace any older deferred question (drop it, sending no answer).
            let _ = self.deferred_brain_question.take();
            self.deferred_brain_question = Some((question, options, response_tx));
            return Ok(());
        }

        self.show_brain_question_dialog(question, options, response_tx)
            .await
    }

    /// Actually show the brain question dialog in TUI and store the response channel.
    async fn show_brain_question_dialog(
        &mut self,
        question: String,
        options: Vec<String>,
        response_tx: tokio::sync::oneshot::Sender<String>,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        // Drop any previous pending brain question (replaced by this new one).
        let _ = self.pending_brain_question_tx.take();
        self.pending_brain_question_options.clear();

        let dialog = if options.is_empty() {
            Dialog::text_input(question, None)
        } else {
            let dialog_options: Vec<DialogOption> = options
                .iter()
                .map(|s| DialogOption::new(s.as_str()))
                .collect();
            Dialog::select(question, dialog_options)
        };

        // Show the dialog in TUI.
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);
        if let Err(e) = tui.render() {
            tracing::error!("[EVENT_LOOP] Failed to render brain question dialog: {}", e);
        }
        drop(tui);

        // Store the response channel and options; the render tick will send the answer.
        self.pending_brain_question_tx = Some(response_tx);
        self.pending_brain_question_options = options;
        Ok(())
    }

    /// Show any deferred brain question if the user is now idle.
    /// Called when a query completes so the brain can ask its question.
    pub(super) async fn maybe_show_deferred_brain_question(&mut self) -> Result<()> {
        if let Some((question, options, response_tx)) = self.deferred_brain_question.take() {
            tracing::debug!("[EVENT_LOOP] Showing deferred brain question now that user is idle");
            self.show_brain_question_dialog(question, options, response_tx)
                .await?;
        }
        Ok(())
    }

    /// Handle a `BrainProposedAction` event: show a Yes/No approval dialog.
    ///
    /// The response channel is stored and resolved by the render tick after the
    /// user makes a selection.  A previously pending action is denied automatically
    /// (replaced by the new one).
    async fn handle_brain_proposed_action(
        &mut self,
        command: String,
        reason: String,
        response_tx: tokio::sync::oneshot::Sender<Option<String>>,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        tracing::debug!("[EVENT_LOOP] Brain proposed action: {}", command);

        // Deny any previously pending action (replaced by this one).
        if let Some(old_tx) = self.pending_brain_action_tx.take() {
            let _ = old_tx.send(None);
        }
        self.pending_brain_action_command = None;

        let prompt = if reason.is_empty() {
            format!("Brain wants to run:\n  `{}`", command)
        } else {
            format!("Brain wants to run:\n  `{}`\n\nReason: {}", command, reason)
        };

        let dialog = Dialog::select(
            prompt,
            vec![
                DialogOption::new("Yes, run it"),
                DialogOption::new("No, skip"),
            ],
        );

        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);
        if let Err(e) = tui.render() {
            tracing::error!("[EVENT_LOOP] Failed to render brain action dialog: {}", e);
        }
        drop(tui);

        self.pending_brain_action_tx = Some(response_tx);
        self.pending_brain_action_command = Some(command);
        Ok(())
    }
}

// ── Daemon brain command handlers ─────────────────────────────────────────────

impl EventLoop {
    /// Register the frontend's home Brain in the daemon's durable named store.
    /// Register the frontend's home namespace and maintain its expiring
    /// environment-runner lease. A failed renewal immediately removes the
    /// runner claim from the status bar; retry never reuses an expired ID.
    async fn register_home_brain(&self) -> Result<Option<std::result::Result<(), String>>> {
        let Some(base) = self.daemon_base_url.as_deref() else {
            return Ok(None);
        };
        let http = reqwest::Client::new();
        let snapshot = http
            .get(format!("{base}/v1/brains/named/{}", self.session_label))
            .send()
            .await?
            .error_for_status()?
            .json::<crate::brain::shared::BrainSnapshot>()
            .await?;
        let endpoint = format!(
            "{base}/v1/brains/named/{}/runner-lease",
            self.session_label
        );
        let initial = request_home_runner_lease(
            &http,
            &endpoint,
            &self.participant_subject,
            &snapshot.environment,
            None,
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
                    .map(|_| ())
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
        tokio::spawn(async move {
            let mut had_lease = initial_had_lease;
            loop {
                tokio::select! {
                    _ = event_tx.closed() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                }
                match request_home_runner_lease(
                    &http,
                    &endpoint,
                    &subject,
                    &environment,
                    lease_id,
                )
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
                        // A retry with an expired/stale lease must acquire a
                        // fresh identity; it may never revive the old lease.
                        lease_id = None;
                        if had_lease {
                            let _ = event_tx.send(ReplEvent::HomeRunnerLeaseStatus {
                                lease_id: None,
                                detail: error.to_string(),
                            });
                        }
                        had_lease = false;
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
        let password = crate::config::load_config()
            .map(|config| config.server.brain_password)
            .unwrap_or_default();
        let mut client = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        client
            .attach(
                &self.participant_subject,
                crate::brain::shared::AttachmentRole::Driver,
                None,
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
        tokio::spawn(async move {
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
                let mut lines = vec![
                    "Named Brains (event revision · retained program stack):".to_string(),
                ];
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
        match reqwest::Client::new()
            .delete(format!("{base}/v1/brains/named/{name}"))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                let destination = body["archived_to"].as_str().unwrap_or("in-memory archive");
                self.output_manager
                    .write_info(format!("archived Brain {name} → {destination}"));
            }
            Ok(response) => self.output_manager.write_info(format!(
                "could not archive Brain {name}: {}",
                response.text().await.unwrap_or_default()
            )),
            Err(error) => self
                .output_manager
                .write_info(format!("could not archive Brain {name}: {error}")),
        }
        self.render_tui().await
    }
}

async fn request_home_runner_lease(
    http: &reqwest::Client,
    endpoint: &str,
    subject: &str,
    environment: &crate::brain::shared::BrainEnvironment,
    lease_id: Option<crate::brain::shared::RunnerLeaseId>,
) -> Result<crate::brain::shared::BrainRunnerLease> {
    let response = http
        .post(endpoint)
        .json(&serde_json::json!({
            "subject": subject,
            "environment": environment,
            "lease_id": lease_id,
            "ttl_ms": 30_000,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(response.text().await.unwrap_or_else(|_| {
            "runner lease request failed".into()
        }));
    }
    Ok(response.json().await?)
}
