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
        // Extract words from the partial input and show arrows in the panel.
        {
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
    fn ipc(&self) -> Option<&crate::ipc::IpcClient> {
        self.ipc_client.as_ref()
    }

    /// Handle `/brain <task>` — spawn a brain in the daemon.
    async fn handle_brain_spawn(&mut self, task: String) -> Result<()> {
        let Some(ipc) = self.ipc() else {
            self.output_manager
                .write_info("⚠️  Daemon not connected — brain sessions require the daemon.");
            return self.render_tui().await;
        };

        match ipc.spawn_brain(&task, None).await {
            Ok(id) => {
                let name: String = task.chars().take(30).collect();
                self.output_manager.write_info(format!(
                    "🧠 Brain '{}' started (id: {})",
                    name, id
                ));
                self.known_brain_states.insert(id, crate::server::BrainState::Running);
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
        let Some(ipc) = self.ipc() else {
            self.output_manager.write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };

        match ipc.list_brains().await {
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
                    lines.push(format!("  {:30}  {}  {}s", b.name, state_str, b.age_secs));
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
        let Some(ipc) = self.ipc() else {
            self.output_manager.write_info("⚠️  Daemon not connected.");
            return self.render_tui().await;
        };

        let id = if let Ok(id) = name_or_id.parse::<uuid::Uuid>() {
            id
        } else {
            match ipc.list_brains().await {
                Ok(brains) => match brains.iter().find(|b| b.name == name_or_id) {
                    Some(b) => b.id,
                    None => {
                        self.output_manager
                            .write_info(format!("⚠️  No brain named '{}'.", name_or_id));
                        return self.render_tui().await;
                    }
                },
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to list brains: {}", e));
                    return self.render_tui().await;
                }
            }
        };

        match self.ipc_client.as_ref().unwrap().cancel_brain(id).await {
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
        if self.ipc_client.is_none() {
            return Ok(());
        }

        // Phase 1: fetch list (borrow dropped after block)
        let brains = {
            match self.ipc_client.as_ref().unwrap().list_brains().await {
                Ok(b) => b,
                Err(_) => return Ok(()), // daemon not reachable — non-fatal
            }
        };

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

        let live_ids: std::collections::HashSet<Uuid> = brains.iter().map(|b| b.id).collect();
        self.known_brain_states.retain(|id, _| live_ids.contains(id));

        struct Transition {
            id: Uuid,
            name: String,
            state: crate::server::BrainState,
        }
        let mut transitions: Vec<Transition> = Vec::new();
        for summary in &brains {
            let prev = self.known_brain_states.get(&summary.id).cloned();
            let new_state = summary.state.clone();
            self.known_brain_states.insert(summary.id, new_state.clone());
            if prev.map(|p| p != new_state).unwrap_or(false) {
                transitions.push(Transition {
                    id: summary.id,
                    name: summary.name.clone(),
                    state: new_state,
                });
            }
        }

        // Phase 2: fetch details for UI-relevant transitions (borrow dropped after each block)
        struct Detail {
            id: Uuid,
            name: String,
            question: Option<crate::server::brain_registry::PendingQuestionView>,
            plan: Option<crate::server::brain_registry::PendingPlanView>,
            final_summary: Option<String>,
        }
        let mut details: Vec<Detail> = Vec::new();
        for t in &transitions {
            match &t.state {
                crate::server::BrainState::WaitingForInput
                | crate::server::BrainState::PlanReady => {
                    if let Ok(d) = self.ipc_client.as_ref().unwrap().get_brain(t.id).await {
                        details.push(Detail {
                            id: t.id,
                            name: t.name.clone(),
                            question: d.pending_question,
                            plan: d.pending_plan,
                            final_summary: None,
                        });
                    }
                }
                crate::server::BrainState::Dead => {
                    if let Ok(d) = self.ipc_client.as_ref().unwrap().get_brain(t.id).await {
                        details.push(Detail {
                            id: t.id,
                            name: t.name.clone(),
                            question: None,
                            plan: None,
                            final_summary: d.final_summary,
                        });
                    }
                }
                _ => {}
            }
        }

        // Phase 3: show dialogs / inject summaries (needs &mut self, no ipc_client borrow held)
        for d in details {
            if let Some(q) = d.question {
                self.show_daemon_brain_question(d.id, &d.name, q).await?;
            } else if let Some(p) = d.plan {
                self.show_daemon_brain_plan(d.id, &d.name, p).await?;
            } else if let Some(summary) = d.final_summary {
                // Brain finished — inject its summary so the next query benefits from it.
                *self.brain_context.write().await = Some(summary.clone());
                self.output_manager.write_info(format!(
                    "🧠 Brain '{}' finished — context ready.",
                    d.name
                ));
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
            let opts: Vec<DialogOption> = q.options.iter().map(DialogOption::new).collect();
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
