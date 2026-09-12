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

    /// Handle `/model <name>` — switch the active named provider profile.
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
        if target_index == active_index && self.model_selection.pending_index().await.is_none() {
            self.output_manager
                .write_info(format!("Already using model: {}", entry.profile_name()));
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
                    let generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::daemon_local::DaemonLocalGenerator::new(
                            client,
                            entry.profile_name(),
                        ));
                    self.model_selection.activate(target_index, generator).await;
                    self.output_manager.write_info(format!(
                        "✓ Switched to {} · {} (conversation preserved)",
                        entry.profile_name(),
                        model
                    ));
                }
                Ok(crate::client::LocalModelStatus::Initializing)
                | Ok(crate::client::LocalModelStatus::Downloading(_))
                | Ok(crate::client::LocalModelStatus::Loading(_)) => {
                    let token = self.model_selection.begin_pending(target_index).await;
                    self.output_manager.write_info(format!(
                        "⏳ {} is still starting; the current model stays active until it is ready.",
                        entry.profile_name()
                    ));

                    let local_generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::daemon_local::DaemonLocalGenerator::new(
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
                    self.output_manager.write_info(format!(
                        "✓ Switched to {} · {} (conversation preserved)",
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
