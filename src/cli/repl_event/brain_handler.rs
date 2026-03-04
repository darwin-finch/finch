// In-process and daemon brain session handlers for EventLoop.
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
// impl EventLoop (daemon brain):
//   handle_brain_spawn      — /brain <task>: spawns brain in daemon
//   handle_brains_list      — /brains: lists active daemon brains
//   handle_brain_cancel     — /brain cancel <id>: cancels a daemon brain
//   poll_daemon_brains      — 500ms poll: detects state transitions
//   update_brain_status_bar — updates status bar brain count
//   show_daemon_brain_question — shows question dialog from daemon brain
//   show_daemon_brain_plan  — shows plan approval dialog from daemon brain

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

    /// Handle a `TypingStarted` event: (re-)spawn the brain with the new partial input.
    async fn handle_typing_started(&self, partial: String) {
        // No-op if brain is disabled or no cloud provider is available.
        let provider = match &self.brain_provider {
            Some(p) => Arc::clone(p),
            None => return,
        };

        // Skip commands and very short input (not worth speculating on).
        if partial.trim().starts_with('/') || partial.trim().len() < 10 {
            return;
        }

        // Cancel stale brain AND clear its context (it was for a different partial input).
        self.cancel_active_brain(true).await;

        let session = crate::brain::BrainSession::spawn(
            partial,
            provider,
            self.event_tx.clone(),
            Arc::clone(&self.brain_context),
            self.cwd.clone(),
            self.memory_system.clone(),
        );

        *self.active_brain.write().await = Some(session);
        tracing::debug!("[EVENT_LOOP] Brain spawned for typing-started event");
    }

    /// Handle a `BrainQuestion` event: show a dialog and store the response channel.
    async fn handle_brain_question(
        &mut self,
        question: String,
        options: Vec<String>,
        response_tx: tokio::sync::oneshot::Sender<String>,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        tracing::debug!("[EVENT_LOOP] Brain question: {}", question);

        // Drop any previous pending brain question (replaced by this new one).
        // The old oneshot sender is dropped here, sending "[no answer]" implicitly.
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
    /// Handle `/brain <task>` — spawn a brain in the daemon.
    async fn handle_brain_spawn(&mut self, task: String) -> Result<()> {
        let Some(ref client) = self.daemon_client else {
            self.output_manager
                .write_info("⚠️  Daemon not connected — brain sessions require the daemon.");
            return self.render_tui().await;
        };

        match client.spawn_brain(&task, None).await {
            Ok(summary) => {
                self.output_manager.write_info(format!(
                    "🧠 Brain '{}' started (id: {})",
                    summary.name, summary.id
                ));
                // Seed known state
                self.known_brain_states.insert(summary.id, summary.state);
                // Immediately update status bar
                self.update_brain_status_bar().await;
            }
            Err(e) => {
                self.output_manager
                    .write_info(format!("⚠️  Failed to spawn brain: {}", e));
            }
        }

        self.render_tui().await
    }

    /// Handle `/brains` — list active brains.
    async fn handle_brains_list(&mut self) -> Result<()> {
        let Some(ref client) = self.daemon_client else {
            self.output_manager
                .write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };

        match client.list_brains().await {
            Ok(brains) if brains.is_empty() => {
                self.output_manager.write_info("No active brain sessions.");
            }
            Ok(brains) => {
                let mut lines = vec!["Active brain sessions:".to_string()];
                for b in &brains {
                    let state_str = match b.state {
                        crate::server::BrainState::Running => "running",
                        crate::server::BrainState::WaitingForInput => "waiting for input",
                        crate::server::BrainState::PlanReady => "plan ready",
                        crate::server::BrainState::Dead => "dead",
                    };
                    lines.push(format!(
                        "  {:30}  {}  {}s",
                        b.name, state_str, b.age_secs
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

    /// Handle `/brain cancel <name-or-id>`.
    async fn handle_brain_cancel(&mut self, name_or_id: String) -> Result<()> {
        let Some(ref client) = self.daemon_client else {
            self.output_manager
                .write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };

        // Try to parse as UUID first; fall back to name lookup via list
        let id = if let Ok(id) = name_or_id.parse::<uuid::Uuid>() {
            id
        } else {
            // Find by name
            match client.list_brains().await {
                Ok(brains) => {
                    match brains.iter().find(|b| b.name == name_or_id) {
                        Some(b) => b.id,
                        None => {
                            self.output_manager.write_info(format!(
                                "⚠️  No brain named '{}'.",
                                name_or_id
                            ));
                            return self.render_tui().await;
                        }
                    }
                }
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to list brains: {}", e));
                    return self.render_tui().await;
                }
            }
        };

        match client.cancel_brain(id).await {
            Ok(()) => {
                self.known_brain_states.remove(&id);
                self.output_manager
                    .write_info(format!("🧠 Brain {} cancelled.", id));
                self.update_brain_status_bar().await;
            }
            Err(e) => {
                self.output_manager
                    .write_info(format!("⚠️  Failed to cancel brain: {}", e));
            }
        }

        self.render_tui().await
    }

    // -------------------------------------------------------------------------
    // Brain polling
    // -------------------------------------------------------------------------

    /// Poll daemon for active brain state transitions (called every 500ms).
    async fn poll_daemon_brains(&mut self) -> Result<()> {
        // Clone the client Arc so we don't hold a borrow on self during async calls.
        let client = match &self.daemon_client {
            Some(c) => Arc::clone(c),
            None => return Ok(()),
        };

        let brains = match client.list_brains().await {
            Ok(b) => b,
            Err(_) => return Ok(()), // daemon not reachable — non-fatal
        };

        // Update status bar brain count
        let active_count = brains.len();
        if active_count > 0 {
            let plural = if active_count == 1 { "brain" } else { "brains" };
            self.status_bar.update_line(
                crate::cli::status_bar::StatusLineType::Custom("brains".to_string()),
                format!("🧠 {} {}", active_count, plural),
            );
        } else {
            self.status_bar
                .remove_line(&crate::cli::status_bar::StatusLineType::Custom("brains".to_string()));
            self.known_brain_states.clear();
            return Ok(());
        }

        // Remove state entries for brains that no longer exist
        let live_ids: std::collections::HashSet<Uuid> = brains.iter().map(|b| b.id).collect();
        self.known_brain_states.retain(|id, _| live_ids.contains(id));

        // Collect transitions we need to act on (avoids re-borrowing self in loop body)
        struct Transition {
            id: Uuid,
            name: String,
            state: crate::server::BrainState,
        }
        let mut transitions: Vec<Transition> = Vec::new();

        for summary in &brains {
            let prev = self.known_brain_states.get(&summary.id).cloned();
            let new_state = summary.state.clone();

            // Update known state
            self.known_brain_states.insert(summary.id, new_state.clone());

            // Detect transitions to WaitingForInput or PlanReady
            let transitioned = prev.map(|p| p != new_state).unwrap_or(false);
            if transitioned {
                transitions.push(Transition {
                    id: summary.id,
                    name: summary.name.clone(),
                    state: new_state,
                });
            }
        }

        // Handle transitions
        for t in transitions {
            match &t.state {
                crate::server::BrainState::WaitingForInput => {
                    if let Ok(detail) = client.get_brain(t.id).await {
                        if let Some(q) = detail.pending_question {
                            self.show_daemon_brain_question(t.id, &t.name, q).await?;
                        }
                    }
                }
                crate::server::BrainState::PlanReady => {
                    if let Ok(detail) = client.get_brain(t.id).await {
                        if let Some(p) = detail.pending_plan {
                            self.show_daemon_brain_plan(t.id, &t.name, p).await?;
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Update status bar brain count from cached state.
    async fn update_brain_status_bar(&self) {
        let n = self.known_brain_states.len();
        if n > 0 {
            let plural = if n == 1 { "brain" } else { "brains" };
            self.status_bar.update_line(
                crate::cli::status_bar::StatusLineType::Custom("brains".to_string()),
                format!("🧠 {} {}", n, plural),
            );
        } else {
            self.status_bar
                .remove_line(&crate::cli::status_bar::StatusLineType::Custom("brains".to_string()));
        }
    }

    /// Show a dialog for a daemon brain question.
    async fn show_daemon_brain_question(
        &mut self,
        brain_id: Uuid,
        brain_name: &str,
        q: crate::server::brain_registry::PendingQuestionView,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        let title = format!("Brain \"{}\" asks:\n{}", brain_name, q.question);

        let dialog = if q.options.is_empty() {
            Dialog::text_input(title, None)
        } else {
            let opts: Vec<DialogOption> = q.options.iter().map(|o| DialogOption::new(o)).collect();
            Dialog::select_with_custom(title, opts)
        };

        // Store context for when dialog result arrives
        self.pending_daemon_brain_id = Some(brain_id);
        self.pending_daemon_brain_question_options = q.options;

        {
            let mut tui = self.tui_renderer.lock().await;
            tui.active_dialog = Some(dialog);
            tui.pending_dialog_result = None;
        }

        // Write question to scrollback
        self.output_manager.write_info(format!(
            "🧠 Brain '{}' asks: {}",
            brain_name, q.question
        ));
        self.render_tui().await
    }

    /// Show a plan approval dialog for a daemon brain.
    async fn show_daemon_brain_plan(
        &mut self,
        brain_id: Uuid,
        brain_name: &str,
        p: crate::server::brain_registry::PendingPlanView,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        let opts = vec![
            DialogOption::new("Approve"),
            DialogOption::new("Request changes"),
            DialogOption::new("Reject"),
        ];
        let mut dialog = Dialog::select(
            format!("Brain \"{}\" plan:", brain_name),
            opts,
        );
        // Show plan content in the dialog body
        dialog.body = Some(p.plan.clone());

        self.pending_daemon_brain_id = Some(brain_id);
        self.pending_daemon_brain_plan = true;
        self.pending_daemon_brain_plan_id = Some(brain_id);

        {
            let mut tui = self.tui_renderer.lock().await;
            tui.active_dialog = Some(dialog);
            tui.pending_dialog_result = None;
        }

        // Write plan to scrollback
        self.output_manager.write_info(format!(
            "🧠 Brain '{}' plan:\n{}",
            brain_name, p.plan
        ));
        self.render_tui().await
    }
}
