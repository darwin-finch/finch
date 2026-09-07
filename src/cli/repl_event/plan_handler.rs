//! Plan-mode tool handlers extracted from the event loop.
//!
//! This module contains three pure free functions that previously lived inside
//! `event_loop.rs`:
//!
//! * [`is_tool_allowed_in_mode`] — gate-check for tools in Planning vs Normal/Executing mode.
//! * [`handle_present_plan`]    — intercepts `PresentPlan` tool calls and shows the approval dialog.
//! * [`handle_ask_user_question`] — intercepts `AskUserQuestion` tool calls and shows a question dialog.
//!
//! All three functions are pure in the sense that they take explicit arguments
//! (no `&self`) and perform no hidden I/O beyond what their parameters provide.
//! That makes them easy to unit-test without standing up a full `EventLoop`.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::cli::messages::work_unit::WorkUnit;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::repl_event::events::ReplEvent;
use crate::cli::tui::TuiRenderer;
use crate::tools::types::ToolUse;

// ── Tool-mode gate ────────────────────────────────────────────────────────────

/// Returns `true` when `tool_name` may be called in `mode`.
///
/// In `Normal` and `Executing` mode all tools are allowed (subject to the
/// normal per-tool confirmation flow).  In `Planning` mode only inspection
/// and plan-completion tools are allowed; `Write`, `Edit`, and similar
/// destructive tools are blocked to enforce read-only exploration.
pub(crate) fn is_tool_allowed_in_mode(tool_name: &str, mode: &ReplMode) -> bool {
    match mode {
        ReplMode::Normal | ReplMode::Executing { .. } => {
            // All tools allowed (subject to normal confirmation)
            true
        }
        ReplMode::Planning { .. } => {
            // Inspection tools, bash (read-only by convention, confirmed normally),
            // plan completion tools, and plan-mode meta-tools are all allowed.
            // Write/Edit remain blocked to enforce read-only exploration during planning.
            matches!(
                tool_name,
                "read"
                    | "glob"
                    | "grep"
                    | "web_fetch"
                    | "bash"
                    | "Bash"
                    | "present_plan"
                    | "PresentPlan"
                    | "ask_user_question"
                    | "AskUserQuestion"
                    // Session-local plan visibility is not a workspace or
                    // host mutation. Keep the familiar checklist usable
                    // while the model is deliberately planning.
                    | "todo_read"
                    | "todo_write"
                    | "TodoRead"
                    | "TodoWrite"
                    | "EnterPlanMode"
                    | "ExitPlanMode"
            )
        }
    }
}

// ── PresentPlan handler ───────────────────────────────────────────────────────

/// Handle a `PresentPlan` tool call by showing an approval dialog.
///
/// Returns `Some(tool_result)` when the tool call is a `PresentPlan` invocation;
/// returns `None` for every other tool name so the caller can fall through to
/// normal tool dispatch.
pub(crate) async fn handle_present_plan(
    tool_use: &ToolUse,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
    mode: Arc<tokio::sync::RwLock<crate::cli::ReplMode>>,
    output_manager: Arc<OutputManager>,
    cancel: CancellationToken,
    work_unit: Arc<WorkUnit>,
    event_tx: &mpsc::UnboundedSender<ReplEvent>,
) -> Option<Result<String>> {
    use chrono::Utc;
    use crossterm::style::Stylize;

    // Accept the canonical wire name and the dispatch-only legacy spelling.
    if !matches!(tool_use.name.as_str(), "present_plan" | "PresentPlan") {
        return None;
    }

    tracing::debug!("[EVENT_LOOP] Detected PresentPlan tool call - showing approval dialog");

    // Extract plan content
    let plan_content = match tool_use.input["plan"].as_str() {
        Some(content) => content,
        None => {
            return Some(Err(anyhow::anyhow!(
                "Missing 'plan' field in PresentPlan input"
            )))
        }
    };

    // Verify we're in planning mode and get plan path
    let (task, plan_path, created_at) = {
        let current_mode = mode.read().await;
        match &*current_mode {
            crate::cli::ReplMode::Planning {
                task,
                plan_path,
                created_at,
            } => (task.clone(), plan_path.clone(), *created_at),
            _ => {
                return Some(Ok(
                    "⚠️  Not in planning mode. Use EnterPlanMode first.".to_string()
                ))
            }
        }
    };

    // Save plan to file
    if let Err(e) = std::fs::write(&plan_path, plan_content) {
        return Some(Err(anyhow::anyhow!("Failed to save plan: {}", e)));
    }

    // Show plan in output
    output_manager.write_info(format!("\n{}\n", "━".repeat(70)));
    output_manager.write_info(format!("{}", "📋 IMPLEMENTATION PLAN".bold()));
    output_manager.write_info(format!("{}\n", "━".repeat(70)));
    output_manager.write_info(plan_content.to_string());
    output_manager.write_info(format!("\n{}\n", "━".repeat(70)));

    // Show approval dialog with full plan in scrollable body.
    let dialog = crate::cli::tui::Dialog::select_with_custom(
        "Review Implementation Plan".to_string(),
        vec![
            crate::cli::tui::DialogOption::with_description(
                "Approve and execute",
                "Proceed with implementation (all tools enabled)",
            ),
            crate::cli::tui::DialogOption::with_description(
                "Request changes",
                "Provide feedback for Claude to revise the plan",
            ),
            crate::cli::tui::DialogOption::with_description(
                "Reject plan",
                "Exit plan mode and return to normal conversation",
            ),
        ],
    )
    .with_body(plan_content.to_string())
    .with_help(
        "↑↓/jk: navigate · Enter: select · o: custom · Ctrl-U/D or PgUp/PgDn: scroll plan · Esc: cancel",
    );

    // Stop the "Deliberating…" spinner before showing the dialog — the model
    // is done thinking; the user now needs to review.
    work_unit.set_complete();

    // Flush plan content to scrollback before showing the dialog overlay so it
    // is visible while the user reviews it.
    {
        let mut tui = tui_renderer.lock().await;
        let _ = tui.flush_output_safe(&output_manager);
    }

    // Belt-and-suspenders: set active_dialog directly so the dialog is on-screen
    // and keypresses route correctly *before* the event loop processes ShowDialog.
    // This eliminates the race window between send() and the event loop's own set.
    {
        let mut tui = tui_renderer.lock().await;
        tui.active_dialog = Some(dialog.clone());
        tui.pending_dialog_result = None;
        let _ = tui.erase_live_area();
        let _ = tui.draw_live_area();
    }
    let (dialog_tx, dialog_rx) = tokio::sync::oneshot::channel::<crate::cli::tui::DialogResult>();
    if event_tx
        .send(ReplEvent::ShowDialog {
            dialog,
            response_tx: dialog_tx,
        })
        .is_err()
    {
        return Some(Ok(dismissed_plan_msg()));
    }

    let dialog_result: crate::cli::tui::DialogResult = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // Clean up active dialog on cancellation
            let mut tui = tui_renderer.lock().await;
            tui.active_dialog = None;
            crate::cli::tui::DialogResult::Cancelled
        }
        result = dialog_rx => match result {
            Ok(r) => r,
            Err(_) => crate::cli::tui::DialogResult::Cancelled,
        },
    };

    // Handle dialog result
    match dialog_result {
        crate::cli::tui::DialogResult::Selected(0) => {
            // Approved — transition to executing mode.
            // Do NOT mutate the conversation here; finalize_tool_execution will add
            // the ToolResult message (referencing the assistant's ToolUse block) after
            // we return.  Adding extra user messages here would create consecutive user
            // messages that the Claude API rejects, causing a silent hang.
            let mut current_mode = mode.write().await;
            let still_owns_planning_mode = matches!(
                &*current_mode,
                crate::cli::ReplMode::Planning {
                    task: current_task,
                    plan_path: current_path,
                    created_at: current_created_at,
                } if current_task == &task
                    && current_path == &plan_path
                    && current_created_at == &created_at
            );
            if cancel.is_cancelled() || !still_owns_planning_mode {
                return Some(Ok(
                    "Plan approval was cancelled before execution authority was granted."
                        .to_string(),
                ));
            }
            *current_mode = crate::cli::ReplMode::Executing {
                task: task.clone(),
                plan_path: plan_path.clone(),
                approved_at: Utc::now(),
            };
            drop(current_mode);

            output_manager.write_info(format!(
                "{}",
                "✓ Plan approved! All tools enabled.".green().bold()
            ));

            // Embed the plan content in the tool result so Claude receives it and
            // knows what to execute next — no extra user message needed.
            Some(Ok(format!(
                "Plan approved by user. Execute this plan step by step:\n\n{}\n\n\
                 All tools are now enabled (Bash, Write, Edit, etc.). Proceed with implementation.",
                plan_content
            )))
        }
        crate::cli::tui::DialogResult::Selected(1)
        | crate::cli::tui::DialogResult::CustomText(_) => {
            // Request changes
            let feedback = if let crate::cli::tui::DialogResult::CustomText(text) = dialog_result {
                Some(text)
            } else {
                None
            };

            output_manager.write_info(format!(
                "{}",
                "📝 Changes requested. Please type your feedback below.".yellow()
            ));

            let msg = if let Some(fb) = feedback {
                format!(
                    "User reviewed the plan and requests the following changes:\n\n{}\n\n\
                     Please revise the implementation plan based on this feedback and call PresentPlan again with the updated version.",
                    fb
                )
            } else {
                "User wants to request changes to the plan. \
                 Please ask the user what changes they would like, then revise the plan and call PresentPlan again with the updated version."
                    .to_string()
            };

            Some(Ok(msg))
        }
        crate::cli::tui::DialogResult::Selected(2) => {
            // Rejected — transition back to normal mode.
            // Do NOT call conversation.add_user_message() here; finalize_tool_execution
            // will add the ToolResult message.  An extra user message here would create
            // consecutive user messages that the Claude API rejects.
            *mode.write().await = crate::cli::ReplMode::Normal;
            output_manager.write_info(format!(
                "{}",
                "✗ Plan rejected. Returning to normal mode.".yellow()
            ));

            Some(Ok(
                "Plan rejected by user. Exiting plan mode and returning to normal conversation."
                    .to_string(),
            ))
        }
        crate::cli::tui::DialogResult::Cancelled => Some(Ok(
            "Plan approval cancelled. Staying in planning mode.".to_string(),
        )),
        _ => Some(Ok("Invalid dialog result.".to_string())),
    }
}

// ── AskUserQuestion handler ───────────────────────────────────────────────────

/// Handle an `AskUserQuestion` tool call by showing a question dialog.
///
/// Returns `Some(tool_result)` when the tool call is an `AskUserQuestion`
/// invocation; returns `None` for every other tool name.
///
/// Single-question dialogs use the non-blocking async overlay path (same as
/// `handle_present_plan`) so `spawn_input_task` can deliver keyboard events
/// while the dialog is open.  Multi-question dialogs fall back to the blocking
/// `show_tabbed_dialog` which takes over the full alternate screen — a separate
/// issue to fix later.
pub(crate) async fn handle_ask_user_question(
    tool_use: &ToolUse,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
    cancel: CancellationToken,
    event_tx: &mpsc::UnboundedSender<ReplEvent>,
) -> Option<Result<String>> {
    // Accept the canonical wire name and the dispatch-only legacy spelling.
    if !matches!(
        tool_use.name.as_str(),
        "ask_user_question" | "AskUserQuestion"
    ) {
        return None;
    }

    tracing::debug!("[EVENT_LOOP] Detected AskUserQuestion tool call");

    // Parse input
    let input: crate::cli::AskUserQuestionInput =
        match serde_json::from_value(tool_use.input.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Some(Err(anyhow::anyhow!(
                    "Failed to parse AskUserQuestion input: {}",
                    e
                )));
            }
        };

    use crate::cli::llm_dialogs;
    use std::collections::HashMap;

    // Multi-question path: show_tabbed_dialog takes over the alternate screen,
    // so the normal event loop is not active.  We still hold the mutex for the
    // duration — this is a known limitation; single-question is the common case.
    if input.questions.len() > 1 {
        let mut tui = tui_renderer.lock().await;
        let tabbed = crate::cli::tui::TabbedDialog::new(input.questions.clone(), None);
        let result = match tui.show_tabbed_dialog(tabbed) {
            Ok(r) => r,
            Err(e) => return Some(Err(anyhow::anyhow!("Failed to show dialog: {}", e))),
        };
        drop(tui);

        let answers = match result {
            crate::cli::tui::TabbedDialogResult::Completed(a) => a,
            crate::cli::tui::TabbedDialogResult::Cancelled => HashMap::new(),
        };
        let annotations = llm_dialogs::build_annotations(&input.questions, &answers);
        let output = crate::cli::AskUserQuestionOutput {
            questions: input.questions.clone(),
            answers,
            annotations,
        };
        if output.answers.is_empty() {
            return Some(Ok(dismissed_msg()));
        }
        return Some(match serde_json::to_string_pretty(&output) {
            Ok(json) => Ok(json),
            Err(e) => Err(anyhow::anyhow!("Failed to serialize output: {}", e)),
        });
    }

    // Single-question — non-blocking overlay path via ShowDialog event + oneshot.
    // The main event loop sets active_dialog; the render tick routes the result
    // back through the oneshot channel.  No 50ms poll needed.
    let question = match input.questions.first() {
        Some(q) => q.clone(),
        None => return Some(Err(anyhow::anyhow!("No questions provided"))),
    };

    let dialog = llm_dialogs::question_to_dialog(&question);

    // Belt-and-suspenders: set active_dialog directly before sending ShowDialog
    // to eliminate the race window where keypresses go to the input buffer.
    {
        let mut tui = tui_renderer.lock().await;
        tui.active_dialog = Some(dialog.clone());
        tui.pending_dialog_result = None;
        let _ = tui.erase_live_area();
        let _ = tui.draw_live_area();
    }
    let (dialog_tx, dialog_rx) = tokio::sync::oneshot::channel::<crate::cli::tui::DialogResult>();
    if event_tx
        .send(ReplEvent::ShowDialog {
            dialog,
            response_tx: dialog_tx,
        })
        .is_err()
    {
        return Some(Ok(dismissed_msg()));
    }

    let dialog_result: crate::cli::tui::DialogResult = tokio::select! {
        result = dialog_rx => match result {
            Ok(r) => r,
            Err(_) => crate::cli::tui::DialogResult::Cancelled,
        },
        _ = cancel.cancelled() => {
            // Clean up active dialog on cancellation
            let mut tui = tui_renderer.lock().await;
            tui.active_dialog = None;
            crate::cli::tui::DialogResult::Cancelled
        }
    };

    if matches!(dialog_result, crate::cli::tui::DialogResult::Cancelled) {
        return Some(Ok(dismissed_msg()));
    }

    let mut answers = HashMap::new();
    if let Some(answer) = llm_dialogs::extract_answer(&question, &dialog_result) {
        answers.insert(question.question.clone(), answer);
    }

    if answers.is_empty() {
        return Some(Ok(dismissed_msg()));
    }

    let annotations = llm_dialogs::build_annotations(&input.questions, &answers);
    let output = crate::cli::AskUserQuestionOutput {
        questions: input.questions.clone(),
        answers,
        annotations,
    };
    Some(match serde_json::to_string_pretty(&output) {
        Ok(json) => Ok(json),
        Err(e) => Err(anyhow::anyhow!("Failed to serialize output: {}", e)),
    })
}

fn dismissed_msg() -> String {
    "The user dismissed the dialog without answering (pressed Escape or cancelled). \
     Do NOT call AskUserQuestion again. Continue without asking, or ask your \
     question inline as plain text in your response."
        .to_string()
}

fn dismissed_plan_msg() -> String {
    "Plan approval cancelled. Staying in planning mode.".to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_tool_allowed_in_mode ───────────────────────────────────────────────
    // Regression: PresentPlan and AskUserQuestion were once missing from the
    // allow-list, causing them to be blocked with "not allowed in planning mode".

    fn planning_mode() -> ReplMode {
        ReplMode::Planning {
            task: String::new(),
            plan_path: std::path::PathBuf::from("/tmp/plan.md"),
            created_at: chrono::Utc::now(),
        }
    }

    async fn resolve_test_plan_dialog(
        selection: crate::cli::tui::DialogResult,
    ) -> (String, std::sync::Arc<tokio::sync::RwLock<ReplMode>>) {
        let temp = tempfile::tempdir().expect("plan dialog fixture needs a temp directory");
        let colors = crate::config::ColorScheme::default();
        let output = std::sync::Arc::new(OutputManager::new(colors.clone()));
        let status = std::sync::Arc::new(crate::cli::StatusBar::new());
        let tui = std::sync::Arc::new(tokio::sync::Mutex::new(TuiRenderer::new_headless(
            std::sync::Arc::clone(&output),
            status,
            colors,
        )));
        let mode = std::sync::Arc::new(tokio::sync::RwLock::new(ReplMode::Planning {
            task: "review plan".into(),
            plan_path: temp.path().join("plan.md"),
            created_at: chrono::Utc::now(),
        }));
        let work_unit = output.start_work_unit("planning");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = tokio::spawn({
            let mode = std::sync::Arc::clone(&mode);
            async move {
                handle_present_plan(
                    &ToolUse {
                        id: "review-plan".into(),
                        name: "present_plan".into(),
                        input: serde_json::json!({"plan": "Review this exact plan."}),
                    },
                    tui,
                    mode,
                    output,
                    tokio_util::sync::CancellationToken::new(),
                    work_unit,
                    &event_tx,
                )
                .await
            }
        });
        let response_tx =
            match tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
                .await
                .expect("plan handler did not publish dialog within one second")
                .expect("plan handler event channel closed")
            {
                ReplEvent::ShowDialog { response_tx, .. } => response_tx,
                event => panic!("expected ShowDialog, got {event:?}"),
            };
        response_tx
            .send(selection)
            .expect("plan handler stopped before dialog selection");
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), handler)
            .await
            .expect("plan dialog resolution timed out")
            .expect("plan dialog task panicked")
            .expect("present_plan handler did not recognize tool")
            .expect("plan review should produce a normal tool result");
        (result, mode)
    }

    #[test]
    fn test_plan_mode_allows_present_plan() {
        let mode = planning_mode();
        assert!(
            is_tool_allowed_in_mode("PresentPlan", &mode),
            "PresentPlan must be allowed in planning mode"
        );
        assert!(
            is_tool_allowed_in_mode("present_plan", &mode),
            "present_plan (snake_case) must be allowed in planning mode"
        );
    }

    #[test]
    fn test_plan_mode_allows_ask_user_question() {
        let mode = planning_mode();
        assert!(
            is_tool_allowed_in_mode("AskUserQuestion", &mode),
            "AskUserQuestion must be allowed in planning mode"
        );
        assert!(
            is_tool_allowed_in_mode("ask_user_question", &mode),
            "ask_user_question (snake_case) must be allowed in planning mode"
        );
    }

    #[test]
    fn test_plan_mode_allows_read_only_tools() {
        let mode = planning_mode();
        for tool in &["read", "glob", "grep", "web_fetch"] {
            assert!(
                is_tool_allowed_in_mode(tool, &mode),
                "{} must be allowed in planning mode",
                tool
            );
        }
    }

    #[test]
    fn test_plan_mode_allows_session_local_todo_projection() {
        let mode = planning_mode();
        for tool in ["todo_read", "todo_write", "TodoRead", "TodoWrite"] {
            assert!(
                is_tool_allowed_in_mode(tool, &mode),
                "{tool} must remain available while planning"
            );
        }
    }

    #[test]
    fn test_plan_mode_blocks_destructive_tools() {
        let mode = planning_mode();
        // Write/Edit are blocked in planning mode to enforce read-only exploration.
        // Bash is allowed (subject to normal confirmation) so the AI can run
        // read-only commands like `which gh`, `cargo check`, etc.
        for tool in &["write", "Write", "edit", "Edit"] {
            assert!(
                !is_tool_allowed_in_mode(tool, &mode),
                "{} must NOT be allowed in planning mode",
                tool
            );
        }
    }

    #[test]
    fn test_plan_mode_allows_bash() {
        let mode = planning_mode();
        assert!(
            is_tool_allowed_in_mode("bash", &mode),
            "bash must be allowed in planning mode (with normal confirmation)"
        );
        assert!(
            is_tool_allowed_in_mode("Bash", &mode),
            "Bash must be allowed in planning mode"
        );
    }

    #[test]
    fn test_plan_mode_allows_enter_exit_plan_mode() {
        let mode = planning_mode();
        assert!(
            is_tool_allowed_in_mode("EnterPlanMode", &mode),
            "EnterPlanMode must be allowed in planning mode"
        );
        assert!(
            is_tool_allowed_in_mode("ExitPlanMode", &mode),
            "ExitPlanMode must be allowed in planning mode"
        );
    }

    #[test]
    fn test_normal_mode_allows_all_tools() {
        let mode = ReplMode::Normal;
        for tool in &[
            "bash",
            "write",
            "edit",
            "PresentPlan",
            "AskUserQuestion",
            "read",
        ] {
            assert!(
                is_tool_allowed_in_mode(tool, &mode),
                "{} must be allowed in normal mode",
                tool
            );
        }
    }

    // ── PresentPlan conversation-structure regression tests (GH Issue #43) ────

    /// Helper: build the conversation that finalize_tool_execution produces after
    /// handle_present_plan returns.  The fixed code produces:
    ///
    ///   assistant { ToolUse { name: "PresentPlan", id: "abc123" } }
    ///   user      { ToolResult { tool_use_id: "abc123", content: "Plan approved..." } }
    ///
    /// which is valid for the Claude API.
    fn build_present_plan_approved_conversation() -> Vec<crate::claude::Message> {
        use crate::claude::{ContentBlock, Message};

        let tool_use_id = "abc123".to_string();

        // 1. Previous user turn that triggered planning.
        let user_msg = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Write a feature X".to_string(),
            }],
        };

        // 2. Assistant response with PresentPlan ToolUse.
        let assistant_msg = Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: tool_use_id.clone(),
                name: "PresentPlan".to_string(),
                input: serde_json::json!({ "plan": "Step 1: …\nStep 2: …" }),
            }],
        };

        // 3. finalize_tool_execution adds a ToolResult user message.
        let tool_result_msg = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: format!(
                    "Plan approved by user. Execute this plan step by step:\n\nStep 1: …\nStep 2: …\n\n\
                     All tools are now enabled (Bash, Write, Edit, etc.). Proceed with implementation."
                ),
                is_error: None,
            }],
        };

        vec![user_msg, assistant_msg, tool_result_msg]
    }

    #[test]
    fn test_present_plan_approve_no_consecutive_user_messages() {
        // Regression for GH #43: the fixed handle_present_plan must not insert
        // extra user messages before the ToolResult.  Consecutive user messages
        // cause the Claude API to return an error → silent hang.
        let msgs = build_present_plan_approved_conversation();

        for window in msgs.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            assert_ne!(
                (a.role.as_str(), b.role.as_str()),
                ("user", "user"),
                "consecutive user messages detected between {:?} and {:?}",
                a,
                b
            );
        }
    }

    #[test]
    fn test_present_plan_approve_tool_result_references_tool_use() {
        // Regression for GH #43: the ToolResult's tool_use_id must reference a
        // ToolUse that exists in the immediately preceding assistant message.
        use crate::claude::ContentBlock;

        let msgs = build_present_plan_approved_conversation();
        assert!(msgs.len() >= 2);

        let last = msgs.last().unwrap();
        assert_eq!(last.role, "user");

        // Collect tool_use_id values from all ToolResult blocks in the last message.
        let result_ids: Vec<&str> = last
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    Some(tool_use_id.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            !result_ids.is_empty(),
            "last message must contain ToolResult blocks"
        );

        // The second-to-last message must be assistant and contain matching ToolUse ids.
        let preceding = &msgs[msgs.len() - 2];
        assert_eq!(preceding.role, "assistant");
        let use_ids: Vec<&str> = preceding
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    Some(id.as_str())
                } else {
                    None
                }
            })
            .collect();

        for rid in &result_ids {
            assert!(
                use_ids.contains(rid),
                "ToolResult references id '{}' but no matching ToolUse found in preceding assistant message; use_ids = {:?}",
                rid,
                use_ids
            );
        }
    }

    #[test]
    fn test_present_plan_approve_invalid_clear_and_add_would_fail() {
        // Documentary test: shows that the OLD buggy pattern (clear conversation,
        // add a plain user message, then add a ToolResult user message) produces
        // consecutive user messages — the invariant the fix avoids.
        use crate::claude::{ContentBlock, Message};

        // Simulate what the buggy code did after Approve + clear_context:
        //   conversation.clear()
        //   conversation.add_user_message("[System: Plan approved!...]")
        //   finalize_tool_execution → adds user { ToolResult { ... } }
        let mut bad_msgs: Vec<Message> = Vec::new();

        // add_user_message produces a user Text message
        bad_msgs.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "[System: Plan approved! Execute this plan:]\n\nStep 1".to_string(),
            }],
        });
        // finalize_tool_execution adds another user message
        bad_msgs.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "abc123".to_string(),
                content: "Plan approved by user...".to_string(),
                is_error: None,
            }],
        });

        // Assert that this old pattern DOES produce consecutive user messages
        // (i.e. the bug is real and our fix is necessary).
        let has_consecutive_users = bad_msgs
            .windows(2)
            .any(|w| w[0].role == "user" && w[1].role == "user");
        assert!(
            has_consecutive_users,
            "expected the old buggy pattern to produce consecutive user messages"
        );
    }

    #[tokio::test]
    async fn issue_363_cancel_wins_over_ready_plan_approval() {
        let temp = tempfile::tempdir().expect("plan cancellation fixture needs a temp directory");
        let colors = crate::config::ColorScheme::default();
        let output = std::sync::Arc::new(OutputManager::new(colors.clone()));
        let status = std::sync::Arc::new(crate::cli::StatusBar::new());
        let tui = std::sync::Arc::new(tokio::sync::Mutex::new(TuiRenderer::new_headless(
            std::sync::Arc::clone(&output),
            status,
            colors,
        )));
        let mode = std::sync::Arc::new(tokio::sync::RwLock::new(ReplMode::Planning {
            task: "cancel plan".into(),
            plan_path: temp.path().join("plan.md"),
            created_at: chrono::Utc::now(),
        }));
        let cancel = tokio_util::sync::CancellationToken::new();
        let work_unit = output.start_work_unit("planning");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let tool_use = ToolUse {
            id: "cancelled-plan".into(),
            name: "present_plan".into(),
            input: serde_json::json!({"plan": "Must not gain execution authority."}),
        };
        let handler = tokio::spawn({
            let tui = std::sync::Arc::clone(&tui);
            let mode = std::sync::Arc::clone(&mode);
            let cancel = cancel.clone();
            let event_tx = event_tx.clone();
            async move {
                handle_present_plan(&tool_use, tui, mode, output, cancel, work_unit, &event_tx)
                    .await
            }
        });
        let response_tx =
            match tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
                .await
                .expect("plan handler did not publish its dialog within one second")
                .expect("plan handler event channel closed before dialog")
            {
                ReplEvent::ShowDialog { response_tx, .. } => response_tx,
                event => panic!("expected ShowDialog before cancellation, got {event:?}"),
            };

        // Make both branches ready without yielding. The biased select and the
        // write-lock recheck must deterministically give cancellation authority.
        cancel.cancel();
        let _ = response_tx.send(crate::cli::tui::DialogResult::Selected(0));
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), handler)
            .await
            .expect("cancelled plan handler did not return within one second")
            .expect("cancelled plan handler task panicked")
            .expect("present_plan handler must recognize the tool")
            .expect("cancellation should be a normal plan result");
        assert!(
            result.contains("cancelled"),
            "cancelled approval must report cancellation, got {result:?}"
        );
        assert!(
            matches!(&*mode.read().await, ReplMode::Planning { .. }),
            "a ready approval must never overwrite cancellation with Executing; mode={:?}",
            mode.read().await
        );
    }

    #[tokio::test]
    async fn issue_363_request_changes_and_reject_preserve_distinct_modes() {
        let (changes, planning) =
            resolve_test_plan_dialog(crate::cli::tui::DialogResult::Selected(1)).await;
        assert!(
            changes.contains("request changes")
                && matches!(&*planning.read().await, ReplMode::Planning { .. }),
            "request changes must retain Planning and ask for revision; result={changes:?}, mode={:?}",
            planning.read().await
        );

        let (rejected, normal) =
            resolve_test_plan_dialog(crate::cli::tui::DialogResult::Selected(2)).await;
        assert!(
            rejected.contains("rejected") && matches!(&*normal.read().await, ReplMode::Normal),
            "reject must return to Normal with an explicit result; result={rejected:?}, mode={:?}",
            normal.read().await
        );
    }
}
