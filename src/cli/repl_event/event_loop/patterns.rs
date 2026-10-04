//! `/patterns` commands: review and revoke standing tool approvals.
//!
//! Every command here reads and changes the tool executor's own confirmation
//! cache (`ToolExecutor` behind `ToolExecutionCoordinator::tool_executor`),
//! which is the store the approval path consults. Nothing is loaded from disk
//! a second time, so a removal takes effect for the very next tool call.
//!
//! These commands are owner-only by construction: they are reached only from
//! `EventLoop::handle_user_input`, which receives the local composer's
//! submitted line. A peer's prompt arrives as a named-Brain turn and is never
//! parsed as a slash command, and a dialog answer comes from the local
//! keyboard only.

use super::*;
use crate::cli::tui::{Dialog, DialogOption};
use crate::tools::{ExactApproval, PatternType, ToolExecutor, ToolPattern, ToolSignature};

/// Removing an approval used more often than this asks for confirmation.
const REMOVE_CONFIRM_MATCH_COUNT: u64 = 10;

/// Shortest ID prefix `/patterns remove` accepts in place of a full ID.
const MIN_ID_PREFIX_LEN: usize = 8;

/// A `/patterns` flow that is waiting on the answer to the dialog it opened.
pub(super) enum PendingPatternsDialog {
    /// "Are you sure?" for `/patterns clear`.
    ConfirmClear,
    /// Confirmation for removing a heavily used approval.
    ConfirmRemove { id: String, noun: &'static str },
    /// One step of the `/patterns add` wizard.
    Add(PatternAddStep),
}

/// The `/patterns add` wizard, one variant per dialog it shows.
pub(super) enum PatternAddStep {
    Type,
    ToolName {
        pattern_type: PatternType,
    },
    Pattern {
        pattern_type: PatternType,
        tool_name: String,
    },
    Description {
        pattern_type: PatternType,
        tool_name: String,
        pattern: String,
    },
    ConfirmTest {
        pattern: Box<ToolPattern>,
    },
    TestString {
        pattern: Box<ToolPattern>,
    },
    ConfirmSave {
        pattern: Box<ToolPattern>,
    },
}

/// One removable standing approval, resolved from a user-supplied ID.
struct RemovalTarget {
    id: String,
    noun: &'static str,
    scope: &'static str,
    tool_name: String,
    matches: String,
    match_count: u64,
}

fn short_id(id: &str) -> &str {
    id.get(..MIN_ID_PREFIX_LEN).unwrap_or(id)
}

fn pattern_type_label(pattern_type: &PatternType) -> &'static str {
    match pattern_type {
        PatternType::Wildcard => "wildcard",
        PatternType::Regex => "regex",
        PatternType::Structured => "structured",
    }
}

fn format_age(duration: chrono::Duration) -> String {
    if duration.num_seconds() < 60 {
        format!("{}s ago", duration.num_seconds().max(0))
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else {
        format!("{}d ago", duration.num_days())
    }
}

fn push_pattern(output: &mut String, pattern: &ToolPattern, scope: &str) {
    let last_used = pattern
        .last_used
        .map(|at| format_age(chrono::Utc::now() - at))
        .unwrap_or_else(|| "never".to_string());
    output.push_str(&format!(
        "  {}  {}  {}\n",
        short_id(&pattern.id),
        scope,
        pattern_type_label(&pattern.pattern_type)
    ));
    output.push_str(&format!("    Tool: {}\n", pattern.tool_name));
    output.push_str(&format!("    Matches: {}\n", pattern.pattern));
    for (label, part) in [
        ("Command", &pattern.command_pattern),
        ("Arguments", &pattern.args_pattern),
        ("Directory", &pattern.dir_pattern),
    ] {
        if let Some(part) = part {
            output.push_str(&format!("    {label}: {part}\n"));
        }
    }
    if !pattern.description.is_empty() {
        output.push_str(&format!("    Description: {}\n", pattern.description));
    }
    output.push_str(&format!(
        "    Match count: {} | Last used: {}\n\n",
        pattern.match_count, last_used
    ));
}

fn push_exact(output: &mut String, approval: &ExactApproval) {
    output.push_str(&format!("  {}  persistent\n", short_id(&approval.id)));
    output.push_str(&format!("    Tool: {}\n", approval.tool_name));
    output.push_str(&format!("    Matches: {}\n", approval.signature));
    output.push_str(&format!("    Match count: {}\n\n", approval.match_count));
}

fn push_session_exact(output: &mut String, signature: &ToolSignature) {
    output.push_str("  (no ID)  session\n");
    output.push_str(&format!("    Tool: {}\n", signature.tool_name));
    output.push_str(&format!("    Matches: {}\n", signature.context_key));
    output.push_str("    Match count: not tracked\n\n");
}

/// Plain-text listing of every standing approval the executor holds.
fn format_patterns_listing(executor: &ToolExecutor) -> String {
    let store = executor.persistent_store();
    let session_patterns = executor.session_patterns();
    let session_exact = executor.session_exact_approvals();
    let mut output = String::from("Tool approval patterns\n\n");

    if store.patterns.is_empty() && session_patterns.is_empty() {
        output.push_str("No patterns configured.\n\n");
    } else {
        output.push_str("Patterns:\n");
        for pattern in &store.patterns {
            push_pattern(&mut output, pattern, "persistent");
        }
        for pattern in session_patterns {
            push_pattern(&mut output, pattern, "session");
        }
    }

    if store.exact_approvals.is_empty() && session_exact.is_empty() {
        output.push_str("No exact approvals configured.\n\n");
    } else {
        output.push_str("Exact approvals:\n");
        for approval in &store.exact_approvals {
            push_exact(&mut output, approval);
        }
        for signature in &session_exact {
            push_session_exact(&mut output, signature);
        }
    }

    output.push_str(&format!(
        "Total: {} patterns ({} persistent, {} session), {} exact approvals ({} persistent, {} session)\n",
        store.patterns.len() + session_patterns.len(),
        store.patterns.len(),
        session_patterns.len(),
        store.exact_approvals.len() + session_exact.len(),
        store.exact_approvals.len(),
        session_exact.len(),
    ));
    output.push_str(
        "Remove one with /patterns remove <id>; a session exact approval has no ID and is \
         removed by /patterns clear or by ending the session.",
    );
    output
}

/// Every approval whose ID is `id` or, for a prefix of at least
/// `MIN_ID_PREFIX_LEN` characters, starts with it.
fn removal_targets(executor: &ToolExecutor, id: &str) -> Vec<RemovalTarget> {
    let selects = |candidate: &str| {
        candidate == id || (id.len() >= MIN_ID_PREFIX_LEN && candidate.starts_with(id))
    };
    let store = executor.persistent_store();
    let pattern_target = |pattern: &ToolPattern, scope: &'static str| RemovalTarget {
        id: pattern.id.clone(),
        noun: "pattern",
        scope,
        tool_name: pattern.tool_name.clone(),
        matches: pattern.pattern.clone(),
        match_count: pattern.match_count,
    };
    let mut targets = Vec::new();
    targets.extend(
        store
            .patterns
            .iter()
            .filter(|pattern| selects(&pattern.id))
            .map(|pattern| pattern_target(pattern, "persistent")),
    );
    targets.extend(
        executor
            .session_patterns()
            .iter()
            .filter(|pattern| selects(&pattern.id))
            .map(|pattern| pattern_target(pattern, "session")),
    );
    targets.extend(
        store
            .exact_approvals
            .iter()
            .filter(|approval| selects(&approval.id))
            .map(|approval| RemovalTarget {
                id: approval.id.clone(),
                noun: "approval",
                scope: "persistent",
                tool_name: approval.tool_name.clone(),
                matches: approval.signature.clone(),
                match_count: approval.match_count,
            }),
    );
    targets
}

fn pattern_type_dialog() -> Dialog {
    Dialog::select(
        "Pattern type:",
        vec![
            DialogOption::with_description(
                "Wildcard (*, **)",
                "Use * for wildcards, ** for recursive paths",
            ),
            DialogOption::with_description("Regex", "Use regular expression syntax"),
        ],
    )
    .with_help("↑↓ or j/k to move, Enter to select, or type 1-2")
}

fn pattern_syntax_help(pattern_type: &PatternType) -> &'static str {
    match pattern_type {
        PatternType::Wildcard => {
            "Pattern syntax:\n  * = match anything (single component)\n  ** = match anything \
             recursively (paths)\nExamples:\n  cargo * in /project\n  reading /project/**\n  \
             cargo * in *"
        }
        PatternType::Regex => {
            "Pattern syntax:\n  Standard regex syntax\nExamples:\n  ^cargo (test|build)$\n  \
             reading /project/src/.*\\.rs$"
        }
        PatternType::Structured => {
            "Pattern syntax:\n  Match command, args, and directory separately\n  Use * to match \
             anything, or specific values"
        }
    }
}

impl EventLoop {
    /// `/patterns` and `/patterns list`: show every standing tool approval.
    pub(super) async fn handle_patterns_list(&mut self) -> Result<()> {
        let listing = {
            let executor = self.tool_coordinator.tool_executor().lock().await;
            format_patterns_listing(&executor)
        };
        self.output_manager.write_info(listing);
        self.render_tui().await
    }

    /// `/patterns remove <id>`: revoke one standing approval and persist.
    pub(super) async fn handle_patterns_remove(&mut self, id: String) -> Result<()> {
        let mut targets = {
            let executor = self.tool_coordinator.tool_executor().lock().await;
            removal_targets(&executor, &id)
        };
        if targets.len() > 1 {
            let candidates = targets
                .iter()
                .map(|target| target.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.output_manager.write_info(format!(
                "ID {id} matches more than one approval ({candidates}). Nothing was removed; \
                 give a longer ID."
            ));
            return self.render_tui().await;
        }
        let Some(target) = targets.pop() else {
            self.output_manager.write_info(format!(
                "No pattern or approval found with ID: {id}\nUse the full ID or at least its \
                 first {MIN_ID_PREFIX_LEN} characters, as shown by /patterns list. Nothing was \
                 removed."
            ));
            return self.render_tui().await;
        };

        self.output_manager.write_info(format!(
            "Found {} to remove:\n  ID: {}\n  Kind: {}\n  Tool: {}\n  Matches: {}\n  Match count: {}",
            target.noun,
            short_id(&target.id),
            target.scope,
            target.tool_name,
            target.matches,
            target.match_count
        ));

        if target.match_count <= REMOVE_CONFIRM_MATCH_COUNT {
            return self.apply_patterns_remove(&target.id, target.noun).await;
        }
        let dialog = Dialog::confirm(
            format!(
                "This {} has been used {} times. Remove?",
                target.noun, target.match_count
            ),
            false,
        );
        self.show_patterns_dialog(
            PendingPatternsDialog::ConfirmRemove {
                id: target.id,
                noun: target.noun,
            },
            dialog,
            "Removal",
        )
        .await
    }

    /// `/patterns clear`: revoke every standing approval, after confirmation.
    pub(super) async fn handle_patterns_clear(&mut self) -> Result<()> {
        let (patterns, exact) = {
            let executor = self.tool_coordinator.tool_executor().lock().await;
            let store = executor.persistent_store();
            (
                store.patterns.len() + executor.session_patterns().len(),
                store.exact_approvals.len() + executor.session_exact_approvals().len(),
            )
        };
        if patterns + exact == 0 {
            self.output_manager.write_info("No patterns to clear.");
            return self.render_tui().await;
        }
        self.output_manager.write_info(format!(
            "This will remove {patterns} pattern(s) and {exact} exact approval(s), persistent \
             and session."
        ));
        self.show_patterns_dialog(
            PendingPatternsDialog::ConfirmClear,
            Dialog::confirm("Are you sure?", false),
            "Clear",
        )
        .await
    }

    /// `/patterns add`: start the wizard that creates a persistent pattern.
    pub(super) async fn handle_patterns_add(&mut self) -> Result<()> {
        self.output_manager.write_info("Add Confirmation Pattern");
        self.show_patterns_dialog(
            PendingPatternsDialog::Add(PatternAddStep::Type),
            pattern_type_dialog(),
            "Pattern creation",
        )
        .await
    }

    /// Open `dialog` for a `/patterns` flow, unless another prompt already
    /// owns the dialog area; in that case nothing is changed.
    async fn show_patterns_dialog(
        &mut self,
        pending: PendingPatternsDialog,
        dialog: Dialog,
        action: &str,
    ) -> Result<()> {
        {
            let mut tui = self.tui_renderer.lock().await;
            if tui.active_dialog.is_some() {
                drop(tui);
                self.output_manager.write_info(format!(
                    "{action} cancelled: another prompt is open. Answer it, then run the \
                     command again. Nothing was changed."
                ));
                return self.render_tui().await;
            }
            tui.active_dialog = Some(dialog);
            tui.pending_dialog_result = None;
        }
        self.pending_patterns_dialog = Some(pending);
        self.render_tui().await
    }

    /// Whether a prompt other than a `/patterns` dialog is waiting for the
    /// next dialog answer. Such a prompt overwrites the on-screen dialog, so
    /// an answer that arrives while one is pending belongs to it.
    async fn another_prompt_awaits_dialog_answer(&self) -> bool {
        if self.pending_dialog_tx.is_some()
            || self.pending_poset_run.is_some()
            || self.active_remote_brain_approval.is_some()
            || self.pending_vm_approval.is_some()
        {
            return true;
        }
        let Some(query_id) = self.active_tool_approval else {
            return false;
        };
        self.pending_approvals.read().await.contains_key(&query_id)
    }

    /// Give a dialog answer to the pending `/patterns` flow, if there is one.
    ///
    /// Returns the answer back when it is not for a `/patterns` dialog, so
    /// the caller routes it to its real owner.
    pub(super) async fn resolve_patterns_dialog(
        &mut self,
        result: crate::cli::tui::DialogResult,
    ) -> Result<Option<crate::cli::tui::DialogResult>> {
        use crate::cli::tui::DialogResult;

        let Some(pending) = self.pending_patterns_dialog.take() else {
            return Ok(Some(result));
        };
        if self.another_prompt_awaits_dialog_answer().await {
            // Another prompt replaced the /patterns dialog on screen. Fail
            // closed: change nothing, and let that prompt have its answer.
            self.output_manager.write_info(
                "The /patterns prompt was replaced by another prompt. Nothing was changed; run \
                 the command again.",
            );
            return Ok(Some(result));
        }

        match (pending, result) {
            (PendingPatternsDialog::ConfirmClear, DialogResult::Confirmed(true)) => {
                self.apply_patterns_clear().await?;
            }
            (PendingPatternsDialog::ConfirmClear, _) => {
                self.output_manager.write_info("Clear cancelled.");
            }
            (PendingPatternsDialog::ConfirmRemove { id, noun }, DialogResult::Confirmed(true)) => {
                self.apply_patterns_remove(&id, noun).await?;
            }
            (PendingPatternsDialog::ConfirmRemove { .. }, _) => {
                self.output_manager.write_info("Removal cancelled.");
            }
            (PendingPatternsDialog::Add(step), result) => {
                self.advance_patterns_add(step, result).await?;
            }
        }
        self.render_tui().await?;
        Ok(None)
    }

    async fn apply_patterns_remove(&mut self, id: &str, noun: &str) -> Result<()> {
        let outcome = {
            let mut executor = self.tool_coordinator.tool_executor().lock().await;
            executor
                .remove_pattern(id)
                .then(|| executor.save_patterns())
        };
        match outcome {
            None => self
                .output_manager
                .write_info(format!("Failed to remove {noun}: {id}")),
            Some(Ok(())) => self
                .output_manager
                .write_info(format!("Removed {noun}: {}", short_id(id))),
            Some(Err(error)) => self.output_manager.write_error(format!(
                "Removed {noun} {} for this session, but saving the pattern store failed: \
                 {error:#}",
                short_id(id)
            )),
        }
        self.render_tui().await
    }

    async fn apply_patterns_clear(&mut self) -> Result<()> {
        let (total, saved) = {
            let mut executor = self.tool_coordinator.tool_executor().lock().await;
            let total = executor.persistent_store().total_count()
                + executor.session_patterns().len()
                + executor.session_exact_approvals().len();
            executor.clear_persistent_patterns();
            executor.clear_session_approvals();
            (total, executor.save_patterns())
        };
        match saved {
            Ok(()) => self
                .output_manager
                .write_info(format!("Cleared {total} pattern(s) and approval(s).")),
            Err(error) => self.output_manager.write_error(format!(
                "Cleared {total} pattern(s) and approval(s) for this session, but saving the \
                 pattern store failed: {error:#}"
            )),
        }
        Ok(())
    }

    /// Consume one answer of the `/patterns add` wizard and show the next
    /// dialog, or finish.
    async fn advance_patterns_add(
        &mut self,
        step: PatternAddStep,
        result: crate::cli::tui::DialogResult,
    ) -> Result<()> {
        use crate::cli::tui::DialogResult;

        const ACTION: &str = "Pattern creation";
        let next = |step| PendingPatternsDialog::Add(step);
        match (step, result) {
            (PatternAddStep::Type, DialogResult::Selected(index)) => {
                let pattern_type = if index == 0 {
                    PatternType::Wildcard
                } else {
                    PatternType::Regex
                };
                let dialog = Dialog::text_input("Tool name:", None)
                    .with_help("bash, read, grep, glob, web_fetch, restart_session");
                self.show_patterns_dialog(
                    next(PatternAddStep::ToolName { pattern_type }),
                    dialog,
                    ACTION,
                )
                .await
            }
            (PatternAddStep::ToolName { pattern_type }, DialogResult::TextEntered(tool_name)) => {
                let tool_name = tool_name.trim().to_string();
                if tool_name.is_empty() {
                    self.output_manager
                        .write_info("Pattern creation cancelled (no tool name).");
                    return Ok(());
                }
                self.output_manager
                    .write_info(pattern_syntax_help(&pattern_type));
                let dialog =
                    Dialog::text_input("Pattern:", None).with_help("Enter the pattern string");
                self.show_patterns_dialog(
                    next(PatternAddStep::Pattern {
                        pattern_type,
                        tool_name,
                    }),
                    dialog,
                    ACTION,
                )
                .await
            }
            (
                PatternAddStep::Pattern {
                    pattern_type,
                    tool_name,
                },
                DialogResult::TextEntered(pattern),
            ) => {
                let pattern = pattern.trim().to_string();
                if pattern.is_empty() {
                    self.output_manager
                        .write_info("Pattern creation cancelled (no pattern).");
                    return Ok(());
                }
                let dialog = Dialog::text_input("Description:", None)
                    .with_help("Brief description of what this pattern allows");
                self.show_patterns_dialog(
                    next(PatternAddStep::Description {
                        pattern_type,
                        tool_name,
                        pattern,
                    }),
                    dialog,
                    ACTION,
                )
                .await
            }
            (
                PatternAddStep::Description {
                    pattern_type,
                    tool_name,
                    pattern,
                },
                DialogResult::TextEntered(description),
            ) => {
                let pattern = ToolPattern::new_with_type(
                    pattern,
                    tool_name,
                    description.trim().to_string(),
                    pattern_type,
                );
                if let Err(error) = pattern.validate() {
                    self.output_manager
                        .write_info(format!("Invalid pattern: {error:#}. Nothing was saved."));
                    return Ok(());
                }
                self.output_manager.write_info(format!(
                    "Pattern created:\n  Tool: {}\n  Pattern: {}\n  Type: {}",
                    pattern.tool_name,
                    pattern.pattern,
                    pattern_type_label(&pattern.pattern_type)
                ));
                self.show_patterns_dialog(
                    next(PatternAddStep::ConfirmTest {
                        pattern: Box::new(pattern),
                    }),
                    Dialog::confirm("Test pattern?", false),
                    ACTION,
                )
                .await
            }
            (PatternAddStep::ConfirmTest { pattern }, DialogResult::Confirmed(true)) => {
                let dialog = Dialog::text_input("Enter test string:", None)
                    .with_help("String to test against the pattern");
                self.show_patterns_dialog(
                    next(PatternAddStep::TestString { pattern }),
                    dialog,
                    ACTION,
                )
                .await
            }
            (PatternAddStep::ConfirmTest { pattern }, DialogResult::Confirmed(false)) => {
                self.show_patterns_dialog(
                    next(PatternAddStep::ConfirmSave { pattern }),
                    Dialog::confirm("Save pattern?", true),
                    ACTION,
                )
                .await
            }
            (PatternAddStep::TestString { pattern }, DialogResult::TextEntered(test)) => {
                let signature = ToolSignature {
                    tool_name: pattern.tool_name.clone(),
                    context_key: test,
                    ..Default::default()
                };
                self.output_manager
                    .write_info(if pattern.matches(&signature) {
                        "Pattern matches the test string."
                    } else {
                        "Pattern does not match the test string."
                    });
                self.show_patterns_dialog(
                    next(PatternAddStep::ConfirmSave { pattern }),
                    Dialog::confirm("Save pattern?", true),
                    ACTION,
                )
                .await
            }
            (PatternAddStep::ConfirmSave { pattern }, DialogResult::Confirmed(true)) => {
                let saved = {
                    let mut executor = self.tool_coordinator.tool_executor().lock().await;
                    executor.approve_pattern_persistent((*pattern).clone());
                    executor.save_patterns()
                };
                match saved {
                    Ok(()) => self.output_manager.write_info(format!(
                        "Pattern saved: {} ({})",
                        short_id(&pattern.id),
                        pattern.pattern
                    )),
                    Err(error) => self.output_manager.write_error(format!(
                        "Pattern {} is active for this session, but saving the pattern store \
                         failed: {error:#}",
                        short_id(&pattern.id)
                    )),
                }
                Ok(())
            }
            // Escape, a declined save, or an answer of the wrong shape:
            // nothing is stored.
            _ => {
                self.output_manager
                    .write_info("Pattern creation cancelled. Nothing was saved.");
                Ok(())
            }
        }
    }
}
