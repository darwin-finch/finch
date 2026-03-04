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
use tokio_util::sync::CancellationToken;

use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
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
) -> Option<Result<String>> {
    use chrono::Utc;
    use crossterm::style::Stylize;

    // Only handle PresentPlan calls
    if tool_use.name != "PresentPlan" {
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
    let (task, plan_path) = {
        let current_mode = mode.read().await;
        match &*current_mode {
            crate::cli::ReplMode::Planning {
                task, plan_path, ..
            } => (task.clone(), plan_path.clone()),
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

    // Build a truncated plan preview for the dialog body (max 30 lines).
    // The full plan is already in scrollback; this lets the user read it without scrolling.
    const PREVIEW_LINES: usize = 30;
    let line_count = plan_content.lines().count();
    let preview_body = if line_count > PREVIEW_LINES {
        let truncated: String = plan_content
            .lines()
            .take(PREVIEW_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "{}\n\n… ({} more lines — ↑ scroll up to see full plan)",
            truncated,
            line_count - PREVIEW_LINES
        )
    } else {
        plan_content.to_string()
    };

    // Show approval dialog
    let dialog = crate::cli::tui::Dialog::select_with_custom(
        "Review Implementation Plan".to_string(),
        vec![
            crate::cli::tui::DialogOption::with_description(
                "Approve and execute",
                "Clear context and proceed with implementation (all tools enabled)",
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
    .with_body(preview_body)
    .with_help(
        "Use ↑↓ or j/k to navigate, Enter to select, 'o' for custom feedback, Esc to cancel",
    );

    // Flush plan content to scrollback before showing the dialog overlay so it
    // is visible while the user reviews it.
    {
        let mut tui = tui_renderer.lock().await;
        let _ = tui.flush_output_safe(&output_manager);
    }

    // Show the approval dialog using the async path so we never hold the tokio
    // async mutex across a blocking crossterm::event::poll syscall.  The old
    // approach (calling show_dialog while holding the mutex) caused the dialog
    // to freeze on macOS because spawn_input_task was suspended waiting for the
    // same mutex, leaving no task free to process keyboard events (GH #43).
    //
    // New approach: set active_dialog, release the mutex, let spawn_input_task
    // handle keypresses normally, and poll here for pending_dialog_result.
    {
        let mut tui = tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);
        tui.pending_dialog_result = None;
        let _ = tui.erase_live_area();
        let _ = tui.draw_live_area();
    }

    let dialog_result: crate::cli::tui::DialogResult = loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Ctrl+C → CancelQuery → cancellation_token.cancel() path.
        // Check before acquiring the mutex to avoid contention.
        if cancel.is_cancelled() {
            let mut tui = tui_renderer.lock().await;
            tui.active_dialog = None;
            break crate::cli::tui::DialogResult::Cancelled;
        }

        let mut tui = tui_renderer.lock().await;
        // Legacy path: pending_cancellation set directly (race with render tick).
        if tui.pending_cancellation {
            tui.pending_cancellation = false;
            tui.active_dialog = None;
            break crate::cli::tui::DialogResult::Cancelled;
        }
        if let Some(result) = tui.pending_dialog_result.take() {
            tui.active_dialog = None;
            break result;
        }
        // Do NOT call draw_live_area() here.  The main event loop's render
        // tick already calls flush_output_safe() → erase_live_area() +
        // draw_live_area() on its own interval.  Calling draw_live_area()
        // here WITHOUT erase_live_area() prints the dialog box to stdout on
        // every 50ms tick, permanently pushing each copy into terminal
        // scrollback and producing the cascading duplicates the user sees.
    };

    // Handle dialog result
    match dialog_result {
        crate::cli::tui::DialogResult::Selected(0) => {
            // Approved — transition to executing mode.
            // Do NOT mutate the conversation here; finalize_tool_execution will add
            // the ToolResult message (referencing the assistant's ToolUse block) after
            // we return.  Adding extra user messages here would create consecutive user
            // messages that the Claude API rejects, causing a silent hang.
            *mode.write().await = crate::cli::ReplMode::Executing {
                task: task.clone(),
                plan_path: plan_path.clone(),
                approved_at: Utc::now(),
            };

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
pub(crate) async fn handle_ask_user_question(
    tool_use: &ToolUse,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
) -> Option<Result<String>> {
    // Only handle AskUserQuestion calls
    if tool_use.name != "AskUserQuestion" {
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

    // Show dialog and collect answers
    let mut tui = tui_renderer.lock().await;
    let result = tui.show_llm_question(&input);
    drop(tui);

    match result {
        Ok(output) => {
            // Empty answers means the user dismissed the dialog (Escape).
            // Return a plain-text message so the model knows to stop asking
            // rather than looping endlessly.
            if output.answers.is_empty() {
                return Some(Ok(
                    "The user dismissed the dialog without answering (pressed Escape or cancelled). \
                     Do NOT call AskUserQuestion again. Continue without asking, or ask your \
                     question inline as plain text in your response."
                        .to_string(),
                ));
            }
            // Serialize output as JSON
            match serde_json::to_string_pretty(&output) {
                Ok(json) => Some(Ok(json)),
                Err(e) => Some(Err(anyhow::anyhow!("Failed to serialize output: {}", e))),
            }
        }
        Err(e) => Some(Err(anyhow::anyhow!("Failed to show LLM question: {}", e))),
    }
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
}
