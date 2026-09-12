use super::*;

impl EventLoop {
    /// Handle user input (query or command)
    pub(super) async fn handle_user_input(&mut self, input: String) -> Result<()> {
        // Check if it's a command
        if input.trim().starts_with('/') {
            // Echo the command to output (like user queries)
            self.output_manager.write_user(input.clone());

            if let Some(command) = Command::parse(&input) {
                match command {
                    Command::Quit => {
                        self.event_tx
                            .send(ReplEvent::Shutdown)
                            .context("Failed to send shutdown event")?;
                        return Ok(());
                    }
                    Command::Help => {
                        let help_text = format_help();
                        self.output_manager.write_info(help_text);
                        self.render_tui().await?;
                    }
                    Command::Setup => {
                        // Suspend the inline TUI, run the full setup wizard,
                        // then resume.  The wizard manages its own terminal
                        // lifecycle (enable_raw_mode / alternate screen).
                        {
                            let tui = self.tui_renderer.lock().await;
                            tui.suspend().ok();
                        }
                        let wizard_result =
                            tokio::task::spawn_blocking(crate::cli::setup_wizard::run_setup_wizard)
                                .await;
                        match wizard_result {
                            Ok(Ok(Some(result))) => {
                                match crate::cli::setup_wizard::validate_repl_and_apply(&result)
                                    .await
                                {
                                    Ok(crate::cli::setup_wizard::SetupApplyOutcome::Saved) => {
                                        self.output_manager.write_info(
                                            "Settings saved. Restart finch to apply changes."
                                                .to_string(),
                                        );
                                    }
                                    Ok(crate::cli::setup_wizard::SetupApplyOutcome::Cancelled) => {
                                        self.output_manager.write_info(
                                            "Setup cancelled; settings were not changed."
                                                .to_string(),
                                        );
                                    }
                                    Err(error) => self
                                        .output_manager
                                        .write_info(format!("Setup was not saved: {error}")),
                                }
                            }
                            Ok(Ok(None)) => {
                                // User cancelled the wizard.
                            }
                            _ => {
                                self.output_manager
                                    .write_info("Setup wizard exited.".to_string());
                            }
                        }
                        // The setup wizard and ChatGPT ceremony own terminal modes for their
                        // complete lifetime. Reacquire the REPL only after every async setup
                        // state is terminal, including cancellation and error recovery.
                        {
                            let mut tui = self.tui_renderer.lock().await;
                            tui.resume().ok();
                        }
                        self.render_tui().await?;
                    }
                    Command::SelfFix => {
                        let prompt = "\
You are running inside your own source directory. \
Your task is to find and fix bugs in yourself.\n\
\n\
Step 1 — diagnose: run `cargo build 2>&1` and capture ALL errors and warnings.\n\
Step 2 — fix: for each error, read the relevant source file, understand the root cause, \
and apply a minimal correct fix using the Edit tool.\n\
Step 3 — check: run `cargo build 2>&1` again. If there are still errors, repeat from step 2.\n\
Step 4 — test: run `cargo test 2>&1`. Fix any failures the same way.\n\
Step 5 — restart: once the build and tests are clean, call restart_session.\n\
\n\
Rules:\n\
- Fix root causes, not symptoms.\n\
- One small edit at a time; verify after each.\n\
- Do not change behaviour — only fix broken code.\n\
- If a fix makes things worse, revert it with another Edit before trying again.";
                        return self.execute_query(prompt.to_string()).await;
                    }
                    Command::Metrics => {
                        use crate::cli::commands::format_metrics;
                        let text = if let Some(ref logger) = self.metrics_logger {
                            match format_metrics(logger) {
                                Ok(s) => s,
                                Err(e) => format!("⚠️  Failed to read metrics: {}", e),
                            }
                        } else {
                            "⚠️  Metrics logger unavailable.".to_string()
                        };
                        self.output_manager.write_info(text);
                        self.render_tui().await?;
                    }
                    Command::Training => {
                        use crate::cli::commands::format_training;
                        let router = Arc::clone(&self.router);
                        let router_ref = router.as_ref();
                        match format_training(Some(router_ref), None) {
                            Ok(s) => self.output_manager.write_info(s),
                            Err(e) => self
                                .output_manager
                                .write_info(format!("⚠️  Failed to read training stats: {}", e)),
                        }
                        self.render_tui().await?;
                    }
                    Command::PersonaList => {
                        let current = self.active_persona.read().await.name().to_string();
                        let mut lines = vec!["Available personas:".to_string()];
                        for name in crate::config::Persona::list_builtins() {
                            let marker = if name.eq_ignore_ascii_case(&current) {
                                "→"
                            } else {
                                " "
                            };
                            lines.push(format!("{marker} {name}"));
                        }
                        lines.push("Use /persona select <name> to switch.".to_string());
                        self.output_manager.write_info(lines.join("\n"));
                        self.render_tui().await?;
                    }
                    Command::PersonaSelect(name) => {
                        match crate::config::Persona::load_by_name(&name) {
                            Ok(persona) => {
                                let old_name = self.active_persona.read().await.name().to_string();
                                *self.active_persona.write().await = persona;
                                self.output_manager
                                    .write_info(format!("Switched persona: {old_name} → {name}"));
                                match crate::config::load_config() {
                                    Ok(mut config) => {
                                        config.active_persona = name.clone();
                                        if let Err(error) = config.save() {
                                            self.output_manager.write_info(format!(
                                                "Could not save persona selection: {error}"
                                            ));
                                        }
                                    }
                                    Err(error) => self.output_manager.write_info(format!(
                                        "Could not load settings to save persona selection: {error}"
                                    )),
                                }
                            }
                            Err(error) => self
                                .output_manager
                                .write_info(format!("Failed to load persona '{name}': {error}")),
                        }
                        self.render_tui().await?;
                    }
                    Command::PersonaShow => {
                        let persona = self.active_persona.read().await;
                        self.output_manager.write_info(format!(
                            "Current persona: {}\n\n{}",
                            persona.name(),
                            persona.behavior.system_prompt
                        ));
                        drop(persona);
                        self.render_tui().await?;
                    }
                    Command::Memory => {
                        use crate::monitoring::MemoryInfo;
                        let info = MemoryInfo::current();
                        self.output_manager.write_info(info.format_with_warning());
                        self.render_tui().await?;
                    }
                    Command::Local { query } => {
                        // Handle /local command - query local model directly (bypass routing)
                        self.handle_local_query(query).await?;
                    }
                    Command::Plan(task) => {
                        self.handle_plan_task(task).await?;
                    }
                    Command::PlanModeToggle => {
                        // Check current mode and toggle
                        let current_mode = self.mode.read().await.clone();
                        match current_mode {
                            ReplMode::Normal => {
                                // Gobble ALL items from the vocabulary stack.
                                // If multiple words have accumulated, drain the whole stack and
                                // stream a plan response — non-blocking so the user can keep
                                // pushing more words while the AI is thinking.
                                // If only one word (or re-planning the stored word), use the full
                                // IMCPD planner for a deeper, multi-iteration plan.
                                let all_words: Vec<String> = {
                                    let mut s = self.stack.lock().await;
                                    std::mem::take(&mut *s)
                                };

                                if all_words.len() >= 2 {
                                    // Multiple concepts — gobble all, stream a combined plan.
                                    self.plan_word = None; // consumed; re-plan starts fresh
                                    let task = format!(
                                        "I've been building a vocabulary: {}. \
                                         Synthesise these concepts into a concrete plan. \
                                         What connects them? What should I build or do?",
                                        all_words.join(", ")
                                    );
                                    self.execute_chat_response(task).await?;
                                } else {
                                    // Single word (or re-plan): full IMCPD plan loop.
                                    let stack_word = if let Some(word) = self.plan_word.clone() {
                                        Some(word)
                                    } else {
                                        all_words.into_iter().next().map(|word| {
                                            self.plan_word = Some(word.clone());
                                            word
                                        })
                                    };

                                    if let Some(task) = stack_word {
                                        // Kick off the full IMPCPD plan loop for the popped word.
                                        self.handle_plan_task(task).await?;
                                    } else {
                                        // No stack word — plain plan mode entry
                                        let plan_path = std::env::temp_dir()
                                            .join(format!("plan_{}.md", uuid::Uuid::new_v4()));
                                        let new_mode = ReplMode::Planning {
                                            task: "Manual exploration".to_string(),
                                            plan_path: plan_path.clone(),
                                            created_at: chrono::Utc::now(),
                                        };
                                        *self.mode.write().await = new_mode.clone();
                                        self.output_manager.write_info(
                                            "📋 Entered plan mode.\n\
                                         You can explore the codebase using read-only tools:\n\
                                         - Read files, glob, grep, web_fetch are allowed\n\
                                         - Write, edit, bash are restricted\n\
                                         Use /plan to exit plan mode.",
                                        );
                                        self.update_plan_mode_indicator(&new_mode);
                                    }
                                } // end single-word else branch
                            }
                            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                                // Exit plan mode, return to normal; clear plan_word
                                *self.mode.write().await = ReplMode::Normal;
                                self.plan_word = None;
                                self.output_manager
                                    .write_info("✅ Exited plan mode. Returned to normal mode.");
                                // Update status bar indicator
                                self.update_plan_mode_indicator(&ReplMode::Normal);
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::McpList => {
                        // List connected MCP servers
                        self.handle_mcp_list().await?;
                    }
                    Command::McpTools(server_filter) => {
                        // List tools from all servers or specific server
                        self.handle_mcp_tools(server_filter).await?;
                    }
                    Command::McpRefresh => {
                        // Refresh tools from all servers
                        self.handle_mcp_refresh().await?;
                    }
                    Command::McpReload => {
                        // Reconnect to all servers
                        self.handle_mcp_reload().await?;
                    }
                    Command::FeedbackCritical(note) => {
                        self.handle_feedback_command(10.0, FeedbackRating::Bad, note)
                            .await?;
                    }
                    Command::FeedbackMedium(note) => {
                        self.handle_feedback_command(3.0, FeedbackRating::Bad, note)
                            .await?;
                    }
                    Command::FeedbackGood(note) => {
                        self.handle_feedback_command(1.0, FeedbackRating::Good, note)
                            .await?;
                    }
                    Command::ModelShow => {
                        self.handle_provider_show().await;
                        self.render_tui().await?;
                    }
                    Command::ModelList => {
                        use crate::providers::create_provider_from_entry;
                        let active = self.model_selection.active_index().await;
                        let pending = self.model_selection.pending_index().await;
                        let mut lines = vec!["Available model profiles:".to_string()];
                        for (index, entry) in self.available_providers.iter().enumerate() {
                            let marker = if index == active {
                                "→"
                            } else if Some(index) == pending {
                                "…"
                            } else {
                                " "
                            };
                            let tag = if entry.is_local() { "local" } else { "cloud" };
                            // Show availability: cloud entries are available if we can build a provider
                            let available =
                                !entry.is_local() && create_provider_from_entry(entry).is_ok();
                            let avail_tag = if entry.is_local() || available {
                                ""
                            } else {
                                " (no API key)"
                            };
                            lines.push(format!(
                                "{} {}. [{}] {} · {}{}",
                                marker,
                                index + 1,
                                tag,
                                entry.profile_name(),
                                entry.model().unwrap_or(entry.provider_type()),
                                avail_tag
                            ));
                        }
                        if self.available_providers.is_empty() {
                            lines.push(
                                "  (none configured — add [[providers]] to ~/.finch/config.toml)"
                                    .to_string(),
                            );
                        }
                        lines.push("Use /model <name> or /model <number> to switch.".to_string());
                        self.output_manager.write_info(lines.join("\n"));
                        self.render_tui().await?;
                    }
                    Command::ModelSwitch(name) => {
                        self.handle_provider_switch(name).await?;
                    }
                    Command::LicenseStatus => {
                        use crate::config::{load_config, LicenseType};
                        let cfg =
                            load_config().unwrap_or_else(|_| crate::config::Config::new(vec![]));
                        let text = match &cfg.license.license_type {
                            LicenseType::Commercial => {
                                let name = cfg.license.licensee_name.as_deref().unwrap_or("(unknown)");
                                let exp = cfg.license.expires_at.as_deref().unwrap_or("(unknown)");
                                format!(
                                    "License: Commercial ✓\n  Licensee: {}\n  Expires:  {}\n  Renew at: https://polar.sh/darwin-finch",
                                    name, exp
                                )
                            }
                            LicenseType::Noncommercial => {
                                "License: Noncommercial\n  Free for personal, educational, and research use.\n  \
                                 Commercial use requires a $10/yr key → https://polar.sh/darwin-finch\n  \
                                 Activate: finch license activate --key <key>".to_string()
                            }
                        };
                        self.output_manager.write_info(text);
                        self.render_tui().await?;
                    }
                    Command::LicenseActivate(key) => {
                        use crate::config::{load_config, LicenseConfig, LicenseType};
                        use crate::license::validate_key;
                        match validate_key(&key) {
                            Ok(parsed) => {
                                if let Ok(mut cfg) = load_config() {
                                    cfg.license = LicenseConfig {
                                        key: Some(key),
                                        license_type: LicenseType::Commercial,
                                        verified_at: Some(
                                            chrono::Local::now().format("%Y-%m-%d").to_string(),
                                        ),
                                        expires_at: Some(
                                            parsed.expires_at.format("%Y-%m-%d").to_string(),
                                        ),
                                        licensee_name: Some(parsed.name.clone()),
                                        notice_suppress_until: None,
                                    };
                                    if let Err(e) = cfg.save() {
                                        self.output_manager.write_info(format!(
                                            "✓ License validated but could not save: {}",
                                            e
                                        ));
                                    } else {
                                        self.output_manager.write_info(format!(
                                            "✓ License activated\n  Licensee: {} ({})\n  Expires:  {}",
                                            parsed.name, parsed.email, parsed.expires_at.format("%Y-%m-%d")
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                self.output_manager
                                    .write_info(format!("✗ License activation failed: {}", e));
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::LicenseRemove => {
                        use crate::config::{load_config, LicenseConfig};
                        if let Ok(mut cfg) = load_config() {
                            cfg.license = LicenseConfig::default();
                            // Removing a licence un-suppressed the notice as a
                            // side effect of writing `notice_suppress_until:
                            // None`. The record lives in a state file now, so
                            // that has to be explicit (#329 review).
                            crate::config::forget_notice_suppression();
                            if let Err(e) = cfg.save() {
                                self.output_manager
                                    .write_info(format!("⚠️  Could not save config: {}", e));
                            } else {
                                self.output_manager.write_info(
                                    "✓ License removed. Now using noncommercial license.",
                                );
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::Brains => {
                        self.handle_brains_list().await?;
                    }
                    Command::BrainArchive(name) => {
                        self.handle_brain_archive(name).await?;
                    }
                    Command::Graph => {
                        self.handle_graph_command().await?;
                    }
                    Command::StackPush(text) => {
                        self.handle_stack_push(text).await?;
                    }
                    Command::StackShow => {
                        self.handle_stack_show().await?;
                    }
                    Command::StackPop => {
                        self.handle_stack_pop().await?;
                    }
                    Command::StackRun => {
                        if let Some(query) = self.handle_stack_run().await? {
                            // confirm_poset_run is called inside handle_poset_or_query.
                            self.handle_poset_or_query(query).await?;
                            {
                                // Placeholder block kept for structure (was: rejected branch).
                                let _ = ();
                                self.render_tui().await?;
                            }
                        }
                    }
                    Command::StackClear => {
                        self.handle_stack_clear().await?;
                    }
                    Command::StackProgram => {
                        self.handle_stack_program().await?;
                    }
                    Command::StackView => {
                        let mut tui = self.tui_renderer.lock().await;
                        if tui.poset_panel_mode == crate::cli::tui::PosetPanelMode::Forth {
                            tui.toggle_poset_view();
                        }
                        drop(tui);
                        self.render_tui().await?;
                    }
                    Command::StackDemo => {
                        self.handle_stack_demo().await?;
                    }
                    Command::StackChain(a, b) => {
                        self.handle_stack_chain(a, b).await?;
                    }
                    Command::StackForget(id) => {
                        self.handle_stack_forget(id).await?;
                    }
                    Command::StackDup(id) => {
                        self.handle_stack_dup(id).await?;
                    }
                    Command::StackSwap(a, b) => {
                        self.handle_stack_swap(a, b).await?;
                    }
                    Command::Ask(query) => {
                        self.execute_query(query).await?;
                    }
                    Command::ForthEval(code) => {
                        if self.selected_brain().is_some() {
                            self.push_remote_brain(crate::brain::store::BrainEventKind::Program {
                                language: crate::brain::store::ProgramLanguage::Forth,
                                source: code,
                            })
                            .await?;
                        } else {
                            self.execute_interactive_typed_program(
                                crate::programs::ProgramLanguage::Forth,
                                code,
                            )
                            .await?;
                        }
                    }
                    Command::BrainAttach(target) => {
                        self.handle_brain_attach(target).await?;
                    }
                    Command::BrainJoin { target, invitation } => {
                        if let Err(error) = self.handle_brain_join(target, invitation).await {
                            self.output_manager
                                .write_info(format!("brain join: {error:#}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainJoinUsage => {
                        self.output_manager.write_info(
                            "Usage: /brain join NAME@MACHINE[:PORT] INVITE\nFor another Brain on this daemon, use: /brain attach NAME",
                        );
                        self.render_tui().await?;
                    }
                    Command::BrainInvite { role, ttl_minutes } => {
                        if let Err(error) = self.handle_brain_invite(role, ttl_minutes).await {
                            self.output_manager
                                .write_info(format!("brain invite: {error:#}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainCreate(target) => {
                        if let Err(error) = self.handle_brain_create(target).await {
                            self.output_manager
                                .write_info(format!("brain create: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainRuns => {
                        self.handle_brain_runs().await?;
                    }
                    Command::BrainInitialize => {
                        if let Err(error) = self.handle_brain_initialize().await {
                            self.output_manager
                                .write_info(format!("brain initialize: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainRunCancel(prefix) => {
                        if let Err(error) = self.handle_brain_run_cancel(prefix).await {
                            self.output_manager
                                .write_info(format!("brain cancel: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainSpeculate(prompt) => {
                        if let Err(error) = self.handle_brain_speculate(prompt).await {
                            self.output_manager
                                .write_info(format!("brain speculate: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainSay(text) => {
                        if let Err(error) = self.handle_brain_say(text).await {
                            self.output_manager
                                .write_info(format!("brain say: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainWho => {
                        if let Err(error) = self.handle_brain_who().await {
                            self.output_manager
                                .write_info(format!("brain who: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainWhois(subject) => {
                        if let Err(error) = self.handle_brain_whois(subject).await {
                            self.output_manager
                                .write_info(format!("brain whois: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainDetach => {
                        self.handle_brain_detach().await?;
                    }
                    Command::BrainHandoff(target) => {
                        if let Err(error) = self.handle_brain_handoff(target).await {
                            self.output_manager
                                .write_info(format!("brain handoff: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainHandoffIdentity => {
                        self.output_manager.write_info(format!(
                            "this frontend's runner identity: {}",
                            self.runner_subject
                        ));
                        self.render_tui().await?;
                    }
                    Command::BrainHandoffAccept(handoff) => {
                        if let Err(error) = self.handle_brain_handoff_accept(handoff).await {
                            self.output_manager
                                .write_info(format!("brain handoff accept: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainHandoffCancel(handoff) => {
                        if let Err(error) = self.handle_brain_handoff_cancel(handoff).await {
                            self.output_manager
                                .write_info(format!("brain handoff cancel: {error}"));
                            self.render_tui().await?;
                        }
                    }
                    Command::BrainPassword(password) => {
                        self.handle_brain_password(password).await?;
                    }
                    Command::Accept(prefix) => {
                        self.handle_accept(prefix).await?;
                    }
                    Command::Reject(reason) => {
                        self.handle_reject(reason).await?;
                    }
                    _ => {
                        // All other commands output to scrollback via write_info
                        self.output_manager.write_info(format!(
                            "Command recognized but not yet implemented: {}",
                            input
                        ));
                        self.render_tui().await?;
                    }
                }
                return Ok(());
            } else {
                self.output_manager
                    .write_info(format!("Unknown command: {input}"));
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Check if it's a quit command (legacy support)
        if input.trim().eq_ignore_ascii_case("quit") || input.trim().eq_ignore_ascii_case("exit") {
            self.event_tx
                .send(ReplEvent::Shutdown)
                .context("Failed to send shutdown event")?;
            return Ok(());
        }

        // `/say` is relay-only while `@finch` explicitly schedules a model
        // turn in a collaborative Brain. The client-side addressee is not
        // persisted in provider context.
        if let Some(prompt) = finch_addressed_prompt(&input) {
            return self.execute_query(prompt.to_string()).await;
        }

        // Forth word definition: `: word ... ;`
        // Route directly to the Forth VM — do not push as a vocabulary word.
        if input.trim().starts_with(": ") {
            if self.selected_brain().is_some() {
                return self
                    .push_remote_brain(crate::brain::store::BrainEventKind::Program {
                        language: crate::brain::store::ProgramLanguage::Forth,
                        source: input,
                    })
                    .await;
            }
            self.output_manager.write_user(input.clone());
            return self
                .execute_interactive_typed_program(
                    crate::programs::ProgramLanguage::Forth,
                    input.trim().to_string(),
                )
                .await;
        }

        // `push <message>` — send plain text to all peers.
        // Direct AI query: `?? question` — bypasses the stack and asks the AI.
        if let Some(query) = input
            .trim()
            .strip_prefix("?? ")
            .or_else(|| input.trim().strip_prefix("??"))
        {
            let query = query.trim().to_string();
            if !query.is_empty() {
                if self.selected_brain().is_none() {
                    self.output_manager.write_user(input.clone());
                }
                return self.execute_query(query).await;
            }
        }

        // ── Lisp: input starting with `(` is a Lisp expression ───────────────
        if input.trim_start().starts_with('(') {
            if self.selected_brain().is_some() {
                return self
                    .push_remote_brain(crate::brain::store::BrainEventKind::Program {
                        language: crate::brain::store::ProgramLanguage::Lisp,
                        source: input,
                    })
                    .await;
            }
            return self
                .execute_interactive_typed_program(crate::programs::ProgramLanguage::Lisp, input)
                .await;
        }

        // Plain terminal text is always a user turn. Executable source is
        // deliberately explicit: Lisp begins with `(`, typed definitions begin
        // with `:`, and other Co-Forth uses `/forth`. Never classify prose by
        // asking the historical semiotic dictionary whether its words exist.
        self.execute_query(input).await
    }
}
