use super::*;

impl EventLoop {
    /// Handle `/accept [prefix]` — apply the patch and notify peers.
    pub(super) async fn handle_accept(&mut self, prefix: Option<String>) -> Result<()> {
        use crossterm::style::Stylize;

        let diff_id = {
            let d = self.diff_store.resolve_pending(prefix.as_deref());
            match d {
                None => {
                    self.output_manager
                        .write_info("no pending diff to accept".dark_grey().to_string());
                    return self.render_tui().await;
                }
                Some(d) => d.id,
            }
        };

        // Mark as accepted in store
        let (label, patch) = {
            if let Some(d) = self.diff_store.accept(diff_id) {
                (d.label.clone(), d.patch.clone())
            } else {
                self.output_manager
                    .write_info("diff not found".dark_grey().to_string());
                return self.render_tui().await;
            }
        };

        // Apply the patch: write the patched content to the file.
        // We implement a simple unified-diff applicator inline.
        let apply_result = self.apply_unified_diff(&label, &patch);
        match apply_result {
            Ok(()) => {
                self.output_manager.write_info(format!(
                    "{}  applied diff to {}",
                    "✓".green(),
                    label.as_str().white(),
                ));
            }
            Err(e) => {
                self.output_manager.write_info(format!(
                    "{}  failed to apply diff to {}: {}",
                    "✗".red(),
                    label.as_str().white(),
                    e,
                ));
            }
        }

        // Publish the review decision to local proposal consumers.
        let _ = self
            .review_tx
            .send(crate::review::ReviewEvent::diff_accept(diff_id));
        self.render_tui().await
    }

    /// Queue the poset run confirmation dialog (non-blocking).
    ///
    /// Sets `active_dialog` and stores the pending run data in `pending_poset_run`.
    /// The render tick executes the poset when the user approves.
    /// Returns `Ok(())` immediately; the approval is handled asynchronously.
    ///
    /// NOTE: The Forth VM show_dialog calls (sync closures ~line 1428–1462) still use
    /// the blocking `show_dialog` path — those require a separate VM refactor.
    pub(super) async fn confirm_poset_run(&mut self) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        let plan = {
            let p = self.poset.lock().await;
            if p.is_empty() {
                return Ok(());
            }

            // Topological sort + depth propagation.
            let mut depth: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0usize)).collect();
            let mut in_deg: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0)).collect();
            for &(_, s) in &p.edges {
                *in_deg.entry(s).or_insert(0) += 1;
            }
            let mut q: std::collections::VecDeque<usize> = in_deg
                .iter()
                .filter(|(_, &d)| d == 0)
                .map(|(&id, _)| id)
                .collect();
            let mut topo: Vec<usize> = Vec::new();
            while let Some(id) = q.pop_front() {
                topo.push(id);
                let d = depth[&id];
                for &(pred, succ) in &p.edges {
                    if pred == id {
                        let e = depth.entry(succ).or_insert(0);
                        if d + 1 > *e {
                            *e = d + 1;
                        }
                        let deg = in_deg.entry(succ).or_insert(0);
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            q.push_back(succ);
                        }
                    }
                }
            }

            let max_depth = depth.values().copied().max().unwrap_or(0);
            let mut lines: Vec<String> = Vec::new();
            for lvl in 0..=max_depth {
                let group: Vec<&crate::poset::Node> = topo
                    .iter()
                    .filter(|&&id| depth[&id] == lvl)
                    .filter_map(|&id| p.nodes.iter().find(|n| n.id == id))
                    .collect();
                if group.is_empty() {
                    continue;
                }

                let concurrent = if group.len() > 1 {
                    "  (concurrent)"
                } else {
                    ""
                };
                lines.push(format!("stage {lvl}{concurrent}"));
                for node in group {
                    let executable = match (&node.compiled_lang, &node.compiled_code) {
                        (Some(language), Some(source)) => {
                            format!(" [{language}: {source}]")
                        }
                        _ if node.tools.is_empty() => " [inference only]".to_string(),
                        _ => format!(" [UNSAFE LEGACY TOOLS: {}]", node.tools.join(", ")),
                    };
                    lines.push(format!("  W{}: {}{}", node.id, node.label, executable));
                }
            }
            lines.join("\n")
        };

        let title = format!("Execution plan\n\n{plan}\n\nThis exact snapshot will run.");
        let dialog = Dialog::select(
            title,
            vec![DialogOption::new("run"), DialogOption::new("cancel")],
        );

        // Store continuation data for the render tick.
        let generator = self.model_selection.generator().await;
        self.pending_poset_run = Some(PendingPosetRun {
            generator,
            poset: self.poset.lock().await.clone(),
            event_tx: self.event_tx.clone(),
        });

        // Show dialog non-blocking (render tick will handle the result).
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.active_dialog = Some(dialog);
            tui.pending_dialog_result = None;
            let _ = tui.erase_live_area();
            let _ = tui.draw_live_area();
        }

        Ok(())
    }

    #[allow(dead_code)]
    /// Handle /plan command - enter planning mode
    pub(super) async fn handle_plan_command(&mut self, task: String) -> Result<()> {
        // Check if already in plan mode
        {
            let mode = self.mode.read().await;
            if matches!(
                *mode,
                ReplMode::Planning { .. } | ReplMode::Executing { .. }
            ) {
                let mode_name = match &*mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                };
                drop(mode);
                self.output_manager.write_info(format!(
                    "⚠️  Already in {} mode. Finish current task first.",
                    mode_name
                ));
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Create plans directory
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        // Generate plan filename
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to planning mode
        let new_mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };
        *self.mode.write().await = new_mode.clone();

        // Update status bar
        self.update_plan_mode_indicator(&new_mode);

        self.output_manager
            .write_info(format!("{}", "✓ Entered planning mode".blue().bold()));
        self.output_manager.write_info(format!("📋 Task: {}", task));
        self.output_manager
            .write_info(format!("📁 Plan will be saved to: {}", plan_path.display()));
        self.output_manager.write_info("");
        self.output_manager
            .write_info(format!("{}", "Available tools:".green()));
        self.output_manager
            .write_info("  read, glob, grep, web_fetch");
        self.output_manager
            .write_info(format!("{}", "Blocked tools:".red()));
        self.output_manager.write_info("  bash, restart_session");
        self.output_manager.write_info("");
        self.output_manager
            .write_info("Ask me to explore the codebase and generate a plan.");
        self.output_manager.write_info(format!(
            "{}",
            "Type /show-plan to view, /approve to execute, /reject to cancel.".dark_grey()
        ));

        // Add mode change notification to conversation
        self.conversation.write().await.add_user_message(format!(
            "[System: Entered planning mode for task: {}]\n\
             Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
             Blocked tools: bash, restart_session\n\
             Please explore the codebase and generate a detailed plan.",
            task
        ));

        self.render_tui().await?;
        Ok(())
    }

    /// Handle `/plan <task>` — run the IMPCPD iterative plan refinement loop.
    ///
    /// 1. Guard against being called while already in Planning/Executing mode.
    /// 2. Transition to `ReplMode::Planning`.
    /// 3. Run the IMPCPD loop (generate → critique → steer, up to 3 iterations).
    /// 4. On convergence or user approval, show the final plan and ask for
    ///    a last confirmation before transitioning to `ReplMode::Executing`.
    pub(super) async fn handle_plan_task(&mut self, task: String) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption, DialogResult};
        use crate::planning::{ImpcpdConfig, PlanLoop, PlanResult};

        // Guard: already planning or executing
        {
            let mode = self.mode.read().await;
            if matches!(
                *mode,
                ReplMode::Planning { .. } | ReplMode::Executing { .. }
            ) {
                let name = match &*mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                };
                drop(mode);
                self.output_manager.write_info(format!(
                    "⚠️  Already in {} mode. Use /plan (no args) to exit first.",
                    name
                ));
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Create plan directory and timestamped path
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to Planning mode
        let planning_mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };
        *self.mode.write().await = planning_mode.clone();
        self.update_plan_mode_indicator(&planning_mode);

        self.output_manager.write_info(format!(
            "{} IMPCPD plan refinement starting\n{} Task: {}",
            "📋",
            " ".repeat(3),
            task.clone().cyan().bold()
        ));
        self.render_tui().await?;

        // ── Run the IMPCPD loop ────────────────────────────────────────────────
        let plan_loop = PlanLoop::new(
            self.model_selection.generator().await,
            Arc::clone(&self.output_manager),
            ImpcpdConfig::default(),
        );
        let result = plan_loop.run(&task, Arc::clone(&self.tui_renderer)).await?;

        // ── Emit convergence summary before the approval dialog ───────────────
        {
            let summary = match &result {
                PlanResult::Converged { iterations } => {
                    let n = iterations.len();
                    let resolved: usize = iterations
                        .iter()
                        .map(|i| i.critiques.iter().filter(|c| c.is_must_address).count())
                        .sum();
                    format!(
                        "{} IMPCPD: {} iteration{}, converged ✓  ({} issues resolved)",
                        "✓".green().bold(),
                        n,
                        if n == 1 { "" } else { "s" },
                        resolved
                    )
                }
                PlanResult::IterationCap { iterations } => {
                    let n = iterations.len();
                    format!(
                        "{} IMPCPD: {} iteration{} — hard cap reached, review carefully",
                        "⚠".yellow().bold(),
                        n,
                        if n == 1 { "" } else { "s" }
                    )
                }
                PlanResult::UserApproved { iterations } => {
                    let n = iterations.len();
                    format!(
                        "{} IMPCPD: {} iteration{}, user-approved mid-loop",
                        "✓".green(),
                        n,
                        if n == 1 { "" } else { "s" }
                    )
                }
                PlanResult::Cancelled => String::new(),
            };
            if !summary.is_empty() {
                self.output_manager.write_info(format!("\n{}\n", summary));
                self.render_tui().await?;
            }
        }

        // ── Handle loop result ────────────────────────────────────────────────
        match result {
            PlanResult::Converged { ref iterations }
            | PlanResult::UserApproved { ref iterations }
            | PlanResult::IterationCap { ref iterations } => {
                let Some(last) = iterations.last() else {
                    *self.mode.write().await = ReplMode::Normal;
                    self.update_plan_mode_indicator(&ReplMode::Normal);
                    self.render_tui().await?;
                    return Ok(());
                };
                let final_plan = last.plan_text.clone();

                // Save final plan to disk
                if let Err(e) = std::fs::write(&plan_path, &final_plan) {
                    self.output_manager
                        .write_info(format!("⚠️  Could not save plan file: {}", e));
                }

                // Show the plan for final human review
                self.output_manager
                    .write_info(format!("\n{}", "━".repeat(70)));
                self.output_manager
                    .write_info(format!("{}", "📋 FINAL IMPLEMENTATION PLAN".bold()));
                self.output_manager
                    .write_info(format!("{}\n", "━".repeat(70)));
                self.output_manager.write_info(final_plan.clone());
                self.output_manager
                    .write_info(format!("\n{}\n", "━".repeat(70)));
                self.render_tui().await?;

                // Final approval dialog
                let approval_dialog = Dialog::select(
                    "Review Final Plan".to_string(),
                    vec![
                        DialogOption::with_description(
                            "Approve and execute",
                            "All tools enabled — proceed with implementation",
                        ),
                        DialogOption::with_description(
                            "Reject",
                            "Exit plan mode without executing",
                        ),
                    ],
                )
                .with_help("↑↓/j/k = navigate · Enter = select · Esc = cancel");

                let approval = {
                    let mut tui = self.tui_renderer.lock().await;
                    tui.show_dialog(approval_dialog)
                        .context("Failed to show approval dialog")?
                };

                match approval {
                    DialogResult::Selected(0) => {
                        // Approved → transition to Executing
                        let exec_mode = ReplMode::Executing {
                            task: task.clone(),
                            plan_path: plan_path.clone(),
                            approved_at: Utc::now(),
                        };
                        *self.mode.write().await = exec_mode.clone();
                        self.update_plan_mode_indicator(&exec_mode);

                        // Replace conversation context with the plan so the LLM
                        // knows exactly what to execute next.
                        self.conversation.write().await.clear();
                        self.conversation.write().await.add_user_message(format!(
                            "[System: Plan approved. Execute this plan step by step:]\n\n{}",
                            final_plan
                        ));

                        self.output_manager.write_info(format!(
                            "{}",
                            "✓ Plan approved! All tools are now enabled.".green().bold()
                        ));
                    }
                    _ => {
                        // Rejected or cancelled
                        *self.mode.write().await = ReplMode::Normal;
                        self.plan_word = None;
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan rejected. Returned to normal mode.");
                    }
                }
            }
            PlanResult::Cancelled => {
                *self.mode.write().await = ReplMode::Normal;
                self.plan_word = None;
                self.update_plan_mode_indicator(&ReplMode::Normal);
                self.output_manager
                    .write_info("Planning cancelled. Returned to normal mode.");
            }
        }

        self.render_tui().await?;
        Ok(())
    }
}
