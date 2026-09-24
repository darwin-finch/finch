use super::*;

impl EventLoop {
    /// Handle feedback commands (/critical, /medium, /good) and Ctrl+G/Ctrl+B quick ratings.
    ///
    /// Finds the last user query and assistant response from conversation history,
    /// logs a `FeedbackEntry` to `~/.finch/feedback.jsonl`, and prints a confirmation.
    pub(super) async fn handle_feedback_command(
        &mut self,
        weight: f64,
        rating: FeedbackRating,
        note: Option<String>,
    ) -> Result<()> {
        let messages = self.conversation.read().await.get_messages();
        let (last_query, last_response) = find_last_exchange(&messages);

        if last_response.is_empty() {
            self.output_manager
                .write_info("No recent response to rate. Ask a question first.");
            self.render_tui().await?;
            return Ok(());
        }

        // Build and log the entry
        let (emoji, label) = match (weight as u64, &rating) {
            (10, _) => ("🔴", "critical (10×)"),
            (3, _) => ("🟡", "medium (3×)"),
            _ => ("🟢", "good (1×)"),
        };

        let mut entry = FeedbackEntry::new(last_query, last_response, rating);
        entry.weight = weight; // Override to support medium (3×)
        if let Some(ref n) = note {
            entry = entry.with_note(n.clone());
        }

        if let Some(ref logger) = self.feedback_logger {
            match logger.log(&entry) {
                Ok(()) => {
                    let msg = if let Some(n) = &note {
                        format!("{} Feedback recorded: {} — {}", emoji, label, n)
                    } else {
                        format!("{} Feedback recorded: {}", emoji, label)
                    };
                    self.output_manager.write_info(msg);
                }
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to log feedback: {}", e));
                }
            }
        } else {
            self.output_manager.write_info(
                "⚠️  Feedback logger unavailable (could not open ~/.finch/feedback.jsonl).",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    fn selection_request(&self) -> crate::cli::repl_event::brain_selection::SelectionRequest {
        crate::cli::repl_event::brain_selection::SelectionRequest {
            default_provider: self.default_provider.clone(),
            persisted: self.brain_selection.clone(),
            cli_provider: self.cli_provider.clone(),
            cli_model: self.cli_model.clone(),
        }
    }

    pub(super) fn effective_selection(
        &self,
    ) -> anyhow::Result<crate::cli::repl_event::brain_selection::EffectiveSelection> {
        crate::cli::repl_event::brain_selection::resolve_selection(
            &self.available_providers,
            &self.selection_request(),
        )
    }

    async fn persist_brain_selection(&mut self) -> Result<()> {
        let persistable = crate::cli::repl_event::brain_selection::persistable_selection(
            &self.effective_selection()?,
            &self.selection_request(),
        );
        if let Some(client) = self.daemon_client.as_ref() {
            let stored = client
                .set_brain_provider_selection(&self.session_label, &persistable)
                .await?;
            self.brain_selection = stored;
        } else {
            self.brain_selection = persistable;
            anyhow::bail!("the Finch daemon is unavailable");
        }
        Ok(())
    }

    async fn apply_effective_selection(&mut self) -> Result<()> {
        let effective = self.effective_selection()?;
        let mut entry = self.available_providers[effective.provider_index].clone();
        if !entry.is_local() {
            entry = entry.with_model_overlay(effective.model.clone());
            entry = entry.with_reasoning_effort_overlay(effective.reasoning_effort);
        }
        if entry.is_local() {
            let Some(client) = self.daemon_client.clone() else {
                anyhow::bail!("Local model switching requires a running Finch daemon.");
            };
            match client.local_model_status().await? {
                crate::client::LocalModelStatus::Ready(_) => {
                    let generator: Arc<dyn Generator> = Arc::new(
                        crate::generators::DaemonLocalGenerator::new(client, entry.profile_name()),
                    );
                    self.model_selection
                        .activate(effective.provider_index, generator)
                        .await;
                }
                crate::client::LocalModelStatus::Initializing
                | crate::client::LocalModelStatus::Downloading(_)
                | crate::client::LocalModelStatus::Loading(_) => {
                    let target_index = effective.provider_index;
                    let local_identity = effective.identity_label();
                    let token = self.model_selection.begin_pending(target_index).await;
                    let active_name = self.model_selection.generator().await.name().to_string();
                    if let Ok(mut tui) = self.tui_renderer.try_lock() {
                        tui.set_model_identity(format!(
                            "{active_name} · active while {} starts",
                            entry.profile_name()
                        ));
                    }
                    let local_generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::DaemonLocalGenerator::new(
                            Arc::clone(&client),
                            entry.profile_name(),
                        ));
                    let selection = self.model_selection.clone();
                    let output = Arc::clone(&self.output_manager);
                    let tui_renderer = Arc::clone(&self.tui_renderer);
                    let profile_name = entry.profile_name();
                    output.write_info(format!(
                        "⏳ {profile_name} is still starting; {active_name} stays active until it is ready."
                    ));
                    // Failure/not-available/status-error mean `fail_pending`
                    // cleared the pending slot and the generator captured in
                    // `active_name` above is still the one serving queries;
                    // the "active while X starts" identity set above must be
                    // reverted so the status bar stops claiming a dead
                    // startup is still in progress. `Cancelled` means a later
                    // switch already owns identity, so it is left alone.
                    let restore_identity = active_name.clone();
                    tokio::spawn(async move {
                        let outcome = activate_local_when_ready(
                            selection,
                            token,
                            target_index,
                            local_generator,
                            || {
                                let client = Arc::clone(&client);
                                async move { client.local_model_status().await }
                            },
                            Duration::from_millis(750),
                        )
                        .await;
                        if let Some(identity) = identity_after_local_activation(
                            &outcome,
                            &local_identity,
                            &restore_identity,
                        ) {
                            tui_renderer.lock().await.set_model_identity(identity);
                        }
                        match outcome {
                            LocalActivationOutcome::Activated(model) => {
                                output.write_info(format!("✓ Switched to {profile_name} · {model}"))
                            }
                            LocalActivationOutcome::Failed(error) => output.write_error(format!(
                                "Local model {profile_name} failed to start: {error}"
                            )),
                            LocalActivationOutcome::NotAvailable => output.write_error(format!(
                                "Local model {profile_name} is not enabled in the daemon"
                            )),
                            LocalActivationOutcome::StatusError(error) => output.write_error(
                                format!("Could not monitor local model {profile_name}: {error}"),
                            ),
                            LocalActivationOutcome::Cancelled => {}
                        }
                    });
                    // Keep the currently usable generator active while the
                    // selected local model downloads and loads. The monitor
                    // atomically switches it once the daemon reports Ready.
                    return Ok(());
                }
                crate::client::LocalModelStatus::Failed(error) => {
                    anyhow::bail!("Local model failed to start: {error}");
                }
                crate::client::LocalModelStatus::NotAvailable => {
                    anyhow::bail!("Local model is not enabled in the daemon");
                }
            }
        } else {
            match self.provider_resolver.resolve_entry(&entry).await {
                Ok(generator) => {
                    self.model_selection
                        .activate(effective.provider_index, generator)
                        .await;
                }
                Err(error) => {
                    anyhow::bail!(
                        "Failed to activate {} · {}: {error}",
                        entry.profile_name(),
                        effective.model.as_deref().unwrap_or(entry.provider_type())
                    );
                }
            }
        }
        self.project_model_identity();
        Ok(())
    }

    fn project_model_identity(&self) {
        if let Ok(effective) = self.effective_selection() {
            let identity = effective.identity_label();
            if let Ok(mut tui) = self.tui_renderer.try_lock() {
                tui.set_model_identity(identity);
            }
        }
    }

    pub(super) async fn hydrate_brain_selection(&mut self) -> Result<()> {
        if let Some(client) = self.daemon_client.as_ref() {
            // The server returns an empty selection for a new Brain. Any
            // error here is therefore a real read failure, not "not found".
            // Stop before applying and persisting defaults, or a transient
            // daemon failure could overwrite an existing Brain selection.
            self.brain_selection = client.brain_provider_selection(&self.session_label).await?;
        }
        if self.brain_selection.provider.is_none() {
            if let Some(default) = self.default_provider.clone() {
                self.brain_selection.provider = Some(default);
                self.brain_selection.provider_inherited = true;
            }
        }
        if let Some(provider) = self.cli_provider.clone() {
            self.brain_selection.provider = Some(provider);
            self.brain_selection.provider_inherited = false;
            self.brain_selection.model = None;
            self.brain_selection.reasoning_effort = None;
        }
        self.apply_effective_selection().await?;
        if self.cli_provider.is_some() || self.brain_selection.provider_inherited {
            self.persist_brain_selection().await?;
        }
        Ok(())
    }

    pub(super) async fn handle_provider_list(&mut self) -> Result<()> {
        let active = self.model_selection.active_index().await;
        let pending = self.model_selection.pending_index().await;
        let mut lines = vec!["Configured provider entries:".to_string()];
        for (index, entry) in self.available_providers.iter().enumerate() {
            let marker = if index == active {
                "→"
            } else if Some(index) == pending {
                "…"
            } else {
                " "
            };
            let tag = if entry.is_local() { "local" } else { "cloud" };
            lines.push(format!(
                "{} {}. [{}] {} · {}",
                marker,
                index + 1,
                tag,
                entry.profile_name(),
                entry.model().unwrap_or(entry.provider_type())
            ));
        }
        if self.available_providers.is_empty() {
            lines.push("  (none configured — add [[providers]] to ~/.finch/config.toml)".into());
        }
        lines.push(
            "Use /provider <name> to bind this Brain. /model overlays a model on the active entry."
                .into(),
        );
        self.output_manager.write_info(lines.join("\n"));
        self.render_tui().await
    }

    pub(super) async fn handle_model_list(&mut self) -> Result<()> {
        let Some(entry) = self
            .available_providers
            .get(self.model_selection.active_index().await)
        else {
            self.output_manager.write_info("No active provider entry.");
            return self.render_tui().await;
        };
        if entry.is_local() {
            self.output_manager.write_info(format!(
                "Local provider '{}' has no ChatGPT-style model picker. Family/size lives on the provider entry. Use /provider to switch entries.",
                entry.profile_name()
            ));
            return self.render_tui().await;
        }
        let current = self
            .effective_selection()
            .ok()
            .and_then(|effective| effective.model)
            .or_else(|| entry.model().map(str::to_string));
        let mut lines = vec![format!(
            "Models for provider '{}' (same credentials):",
            entry.profile_name()
        )];
        if let Some(model) = entry.model() {
            let marker = if current.as_deref() == Some(model) {
                "→"
            } else {
                " "
            };
            lines.push(format!("{marker} {model}"));
        }
        if let Some(current) = current.as_deref() {
            if entry.model() != Some(current) {
                lines.push(format!("→ {current}  (Brain overlay)"));
            }
        }
        lines.push(
            "Use /model <id> to overlay a model on this Brain. This does not switch accounts."
                .into(),
        );
        self.output_manager.write_info(lines.join("\n"));
        self.render_tui().await
    }

    pub(super) async fn handle_model_show(&mut self) -> Result<()> {
        match self.effective_selection() {
            Ok(effective) => self
                .output_manager
                .write_info(effective.status_report(self.default_provider.as_deref())),
            Err(error) => self.output_manager.write_info(format!("⚠️  {error}")),
        }
        self.render_tui().await
    }

    pub(super) async fn handle_status(&mut self) -> Result<()> {
        self.handle_model_show().await
    }

    pub(super) async fn handle_model_overlay(&mut self, model: String) -> Result<()> {
        let Some(entry) = self
            .available_providers
            .get(self.model_selection.active_index().await)
        else {
            self.output_manager.write_info("No active provider entry.");
            return self.render_tui().await;
        };
        if entry.is_local() {
            self.output_manager.write_info(format!(
                "⚠️  Local provider '{}' has no ChatGPT-style model picker. Use /provider to switch entries.",
                entry.profile_name()
            ));
            return self.render_tui().await;
        }
        let previous_selection = self.brain_selection.clone();
        let previous_cli_model = self.cli_model.take();
        self.brain_selection.model = Some(model.trim().to_string());
        self.brain_selection.provider = Some(entry.profile_name());
        self.brain_selection.provider_inherited = false;
        if let Err(error) = self.apply_effective_selection().await {
            self.brain_selection = previous_selection;
            self.cli_model = previous_cli_model;
            self.output_manager
                .write_info(format!("⚠️  Model was not changed: {error}"));
            return self.render_tui().await;
        }
        if let Err(error) = self.persist_brain_selection().await {
            self.output_manager.write_info(format!(
                "⚠️  Model is active for this process but could not be persisted on this Brain: {error}"
            ));
            return self.render_tui().await;
        }
        if let Ok(effective) = self.effective_selection() {
            self.output_manager.write_info(format!(
                "✓ Model overlay {} on {} (persisted on this Brain)",
                effective.model.as_deref().unwrap_or(&model),
                effective.provider_name
            ));
        }
        self.render_tui().await
    }

    pub(super) async fn handle_thinking_show(&mut self) -> Result<()> {
        match self.effective_selection() {
            Ok(effective) => {
                if effective.local || effective.reasoning_effort.is_none() {
                    let entry = &self.available_providers[effective.provider_index];
                    if !entry.supports_reasoning_effort() {
                        self.output_manager.write_info(format!(
                            "Thinking level is unsupported for provider '{}'.",
                            effective.provider_name
                        ));
                    } else {
                        self.output_manager.write_info(
                            "thinking: provider default\nUse /thinking <none|minimal|low|medium|high|xhigh|max>."
                                .to_string(),
                        );
                    }
                } else {
                    self.output_manager.write_info(format!(
                        "thinking: {}",
                        effective.reasoning_effort.unwrap().as_str()
                    ));
                }
            }
            Err(error) => self.output_manager.write_info(format!("⚠️  {error}")),
        }
        self.render_tui().await
    }

    pub(super) async fn handle_thinking_set(&mut self, level: String) -> Result<()> {
        let effective = match self.effective_selection() {
            Ok(effective) => effective,
            Err(error) => {
                self.output_manager.write_info(format!("⚠️  {error}"));
                return self.render_tui().await;
            }
        };
        let entry = &self.available_providers[effective.provider_index];
        if !entry.supports_reasoning_effort() {
            self.output_manager.write_info(format!(
                "⚠️  Thinking level is unsupported for provider '{}'.",
                effective.provider_name
            ));
            return self.render_tui().await;
        }
        match crate::cli::repl_event::brain_selection::parse_reasoning_effort(&level) {
            Ok(effort) => {
                let previous_selection = self.brain_selection.clone();
                self.brain_selection.reasoning_effort = Some(effort.as_str().to_string());
                self.brain_selection.provider = Some(effective.provider_name.clone());
                self.brain_selection.provider_inherited = false;
                if let Err(error) = self.apply_effective_selection().await {
                    self.brain_selection = previous_selection;
                    self.output_manager
                        .write_info(format!("⚠️  Thinking level was not changed: {error}"));
                    return self.render_tui().await;
                }
                if let Err(error) = self.persist_brain_selection().await {
                    self.output_manager.write_info(format!(
                        "⚠️  Thinking level is active for this process but could not be persisted on this Brain: {error}"
                    ));
                    return self.render_tui().await;
                }
                self.output_manager.write_info(format!(
                    "✓ Thinking overlay {} on {} (persisted on this Brain)",
                    effort.as_str(),
                    effective.provider_name
                ));
            }
            Err(error) => self.output_manager.write_info(format!("⚠️  {error}")),
        }
        self.render_tui().await
    }

    /// Handle `/provider <name>` — bind this Brain to a configured provider entry.
    pub(super) async fn handle_provider_switch(&mut self, name: String) -> Result<()> {
        let target_index = match resolve_provider_profile(&self.available_providers, &name) {
            Ok(index) => index,
            Err(error) => {
                self.output_manager.write_info(format!("⚠️  {error}"));
                return self.render_tui().await;
            }
        };
        let entry = self.available_providers[target_index].clone();
        let active_index = self.model_selection.active_index().await;
        if target_index == active_index
            && self.model_selection.pending_index().await.is_none()
            && self.brain_selection.model.is_none()
            && self.brain_selection.reasoning_effort.is_none()
            && self.cli_model.is_none()
        {
            self.brain_selection.provider = Some(entry.profile_name());
            self.brain_selection.provider_inherited = false;
            self.brain_selection.model = None;
            self.brain_selection.reasoning_effort = None;
            self.cli_model = None;
            self.cli_provider = None;
            match self.persist_brain_selection().await {
                Ok(()) => self.output_manager.write_info(format!(
                    "✓ Provider {} is now an explicit Brain override",
                    entry.profile_name()
                )),
                Err(error) => self.output_manager.write_info(format!(
                    "⚠️  Already using {}, but could not persist it on this Brain: {error}",
                    entry.profile_name()
                )),
            }
            return self.render_tui().await;
        }

        // Every valid new selection supersedes a local startup already in flight,
        // even if constructing or checking the replacement later fails.
        self.model_selection.cancel_pending().await;

        if entry.is_local() {
            let Some(client) = self.daemon_client.clone() else {
                self.output_manager.write_info(
                    "⚠️  Local model switching requires a running Finch daemon.".to_string(),
                );
                return self.render_tui().await;
            };

            match client.local_model_status().await {
                Ok(crate::client::LocalModelStatus::Ready(model)) => {
                    let generator: Arc<dyn Generator> = Arc::new(
                        crate::generators::DaemonLocalGenerator::new(client, entry.profile_name()),
                    );
                    self.model_selection.activate(target_index, generator).await;
                    self.brain_selection.provider = Some(entry.profile_name());
                    self.brain_selection.model = None;
                    self.brain_selection.reasoning_effort = None;
                    self.brain_selection.provider_inherited = false;
                    self.cli_model = None;
                    self.cli_provider = None;
                    if let Err(error) = self.persist_brain_selection().await {
                        self.output_manager.write_info(format!(
                            "⚠️  Provider is active for this process but could not be persisted on this Brain: {error}"
                        ));
                        self.project_model_identity();
                        return self.render_tui().await;
                    }
                    self.project_model_identity();
                    self.output_manager.write_info(format!(
                        "✓ Provider {} · {} (persisted on this Brain)",
                        entry.profile_name(),
                        model
                    ));
                }
                Ok(crate::client::LocalModelStatus::Initializing)
                | Ok(crate::client::LocalModelStatus::Downloading(_))
                | Ok(crate::client::LocalModelStatus::Loading(_)) => {
                    self.brain_selection.provider = Some(entry.profile_name());
                    self.brain_selection.model = None;
                    self.brain_selection.reasoning_effort = None;
                    self.brain_selection.provider_inherited = false;
                    self.cli_model = None;
                    self.cli_provider = None;
                    if let Err(error) = self.persist_brain_selection().await {
                        self.output_manager.write_info(format!(
                            "⚠️  Could not persist the pending local provider on this Brain: {error}"
                        ));
                        return self.render_tui().await;
                    }
                    let token = self.model_selection.begin_pending(target_index).await;
                    self.output_manager.write_info(format!(
                        "⏳ {} is still starting; the current model stays active until it is ready.",
                        entry.profile_name()
                    ));

                    let local_generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::DaemonLocalGenerator::new(
                            Arc::clone(&client),
                            entry.profile_name(),
                        ));
                    let selection = self.model_selection.clone();
                    let output = Arc::clone(&self.output_manager);
                    let profile_name = entry.profile_name();
                    tokio::spawn(async move {
                        let outcome = activate_local_when_ready(
                            selection,
                            token,
                            target_index,
                            local_generator,
                            || {
                                let client = Arc::clone(&client);
                                async move { client.local_model_status().await }
                            },
                            Duration::from_millis(750),
                        )
                        .await;
                        match outcome {
                            LocalActivationOutcome::Activated(model) => output.write_info(format!(
                                "✓ Switched to {profile_name} · {model} (conversation preserved)"
                            )),
                            LocalActivationOutcome::Failed(error) => output.write_error(format!(
                                "Local model {profile_name} failed to start: {error}"
                            )),
                            LocalActivationOutcome::NotAvailable => output.write_error(format!(
                                "Local model {profile_name} is not enabled in the daemon"
                            )),
                            LocalActivationOutcome::StatusError(error) => output.write_error(
                                format!("Could not monitor local model {profile_name}: {error}"),
                            ),
                            LocalActivationOutcome::Cancelled => {}
                        }
                    });
                }
                Ok(crate::client::LocalModelStatus::Failed(error)) => {
                    self.output_manager
                        .write_info(format!("⚠️  Local model failed to start: {error}"));
                }
                Ok(crate::client::LocalModelStatus::NotAvailable) => {
                    self.output_manager.write_info(
                        "⚠️  This daemon was started without a local model enabled.".to_string(),
                    );
                }
                Err(error) => {
                    self.output_manager
                        .write_info(format!("⚠️  Could not read local model status: {error}"));
                }
            }
        } else {
            match self
                .provider_resolver
                .resolve(Some(&entry.profile_name()), entry.model())
                .await
            {
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to create model '{}': {}", name, e));
                }
                Ok(new_gen) => {
                    self.model_selection.activate(target_index, new_gen).await;
                    self.brain_selection.provider = Some(entry.profile_name());
                    self.brain_selection.model = None;
                    self.brain_selection.reasoning_effort = None;
                    self.brain_selection.provider_inherited = false;
                    self.cli_model = None;
                    self.cli_provider = None;
                    if let Err(error) = self.persist_brain_selection().await {
                        self.output_manager.write_info(format!(
                            "⚠️  Provider is active for this process but could not be persisted on this Brain: {error}"
                        ));
                        self.project_model_identity();
                        return self.render_tui().await;
                    }
                    self.project_model_identity();
                    self.output_manager.write_info(format!(
                        "✓ Provider {} · {} (persisted on this Brain)",
                        entry.profile_name(),
                        entry.model().unwrap_or(entry.provider_type())
                    ));
                }
            }
        }
        self.render_tui().await
    }

    /// Handle `/program` — render the current stack as Forth source code.
    ///
    /// Seed the plan with a small typed Co-Forth dependency graph as a demo.
    ///
    /// Defines four words:
    ///   W0  TWENTY       — produces 20
    ///   W1  ONE          — produces 1
    ///   W2  TWENTY-ONE   — adds W0 and W1                                     needs W0, W1
    ///   W3  ANSWER       — doubles W2                                         needs W2
    ///
    /// Parallel roots W0 and W1 run concurrently; W2 then W3 consume their typed results.
    pub(super) async fn handle_stack_demo(&mut self) -> Result<()> {
        use crate::poset::{NodeAuthor, NodeKind};

        // Clear any existing stack and poset first.
        self.stack.lock().await.clear();
        {
            let mut p = self.poset.lock().await;
            *p = crate::poset::Poset::new();
        }

        // Reviewed nodes: (label, typed Co-Forth source, predecessors).
        let words: &[(&str, &str, &[usize])] = &[
            ("produce twenty", "20", &[]),
            ("produce one", "1", &[]),
            ("add the two predecessor values", "+", &[0, 1]),
            ("double the predecessor value", "2 *", &[2]),
        ];

        let mut ids: Vec<usize> = Vec::new();
        {
            let mut p = self.poset.lock().await;
            for &(label, source, _) in words {
                let id = p.add_node(label.to_string(), NodeKind::Task, NodeAuthor::User);
                let node = p.node_mut(id).expect("newly added plan node");
                node.compiled_code = Some(source.to_string());
                node.compiled_lang = Some("forth".to_string());
                ids.push(id);
            }
            // Wire edges based on predecessor lists.
            for (i, &(_, _, preds)) in words.iter().enumerate() {
                for &pred_idx in preds {
                    p.edges.push((ids[pred_idx], ids[i]));
                }
            }
        }

        // Mirror into the flat stack (for /stack show compatibility).
        {
            let mut s = self.stack.lock().await;
            for &(label, _, _) in words {
                s.push(label.to_string());
            }
        }

        self.output_manager.write_info(
            "📚 Demo plan seeded: 4 reviewed typed nodes, 3 edges.\n\
             W0 + W1 run in parallel → W2 → W3, producing 42.\n\
             /program to see the vocabulary · /view for graph · /run to execute.",
        );

        // Switch to Forth view so the vocabulary is immediately visible.
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.poset_panel_mode = crate::cli::tui::PosetPanelMode::Forth;
        }
        self.render_tui().await
    }
}

/// What the status bar's model identity should become once a deferred local
/// model activation settles, given the identity it would show once the
/// switch succeeds (`local_identity`) and the identity that was active
/// before the switch was attempted (`previous_identity`).
///
/// `Activated` adopts the new identity. `Failed`, `NotAvailable`, and
/// `StatusError` all mean `fail_pending` cleared the pending slot and the
/// original generator is still serving queries, so the "active while X
/// starts" identity set when the switch began must be reverted — otherwise
/// the status bar keeps claiming a dead startup is still in progress.
/// `Cancelled` means a later switch already owns the identity, so the caller
/// must leave it alone (`None`).
fn identity_after_local_activation(
    outcome: &LocalActivationOutcome,
    local_identity: &str,
    previous_identity: &str,
) -> Option<String> {
    match outcome {
        LocalActivationOutcome::Activated(_) => Some(local_identity.to_string()),
        LocalActivationOutcome::Failed(_)
        | LocalActivationOutcome::NotAvailable
        | LocalActivationOutcome::StatusError(_) => Some(previous_identity.to_string()),
        LocalActivationOutcome::Cancelled => None,
    }
}

#[cfg(test)]
mod local_activation_identity_tests {
    use super::*;

    #[test]
    fn activated_outcome_adopts_the_local_identity() {
        let outcome = LocalActivationOutcome::Activated("Qwen 2.5 3B".to_string());

        let identity = identity_after_local_activation(&outcome, "local · Qwen 2.5 3B", "cloud");

        assert_eq!(
            identity,
            Some("local · Qwen 2.5 3B".to_string()),
            "a successful activation must adopt the new model's identity"
        );
    }

    #[test]
    fn failed_outcome_restores_the_previous_identity() {
        let outcome = LocalActivationOutcome::Failed("bad weights".to_string());

        let identity = identity_after_local_activation(&outcome, "local · Qwen 2.5 3B", "cloud");

        assert_eq!(
            identity,
            Some("cloud".to_string()),
            "a failed startup must stop claiming the dead download is still in progress \
             and revert to the generator that is actually still serving queries"
        );
    }

    #[test]
    fn not_available_outcome_restores_the_previous_identity() {
        let outcome = LocalActivationOutcome::NotAvailable;

        let identity = identity_after_local_activation(&outcome, "local · Qwen 2.5 3B", "cloud");

        assert_eq!(identity, Some("cloud".to_string()));
    }

    #[test]
    fn status_error_outcome_restores_the_previous_identity() {
        let outcome = LocalActivationOutcome::StatusError("daemon unreachable".to_string());

        let identity = identity_after_local_activation(&outcome, "local · Qwen 2.5 3B", "cloud");

        assert_eq!(identity, Some("cloud".to_string()));
    }

    #[test]
    fn cancelled_outcome_leaves_identity_alone() {
        let outcome = LocalActivationOutcome::Cancelled;

        let identity = identity_after_local_activation(&outcome, "local · Qwen 2.5 3B", "cloud");

        assert_eq!(
            identity, None,
            "a superseding switch already owns the identity; this branch must not clobber it"
        );
    }
}
