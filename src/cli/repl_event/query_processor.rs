//! Query processing — routing, streaming, tool dispatch, and sliding-window context.
//!
//! Extracted from `event_loop.rs` to keep that file focused on event dispatch.
//! The key entry point is [`process_query_with_tools`], called as a background
//! Tokio task from [`super::event_loop::EventLoop::spawn_query_task`].

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::claude::ContentBlock;
use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::generators::{Generator, StreamChunk};
use crate::models::bootstrap::GeneratorState;
use crate::router::Router;
use crate::tools::types::{ToolDefinition, ToolUse};

use super::events::ReplEvent;
use super::query_state::{QueryState, QueryStateManager};
use super::tool_execution::ToolExecutionCoordinator;

/// Shared map of active tool calls keyed by tool_id.
/// Maps `tool_id → (tool_name, tool_input, work_unit, row_idx)`.
pub(crate) type ActiveToolUsesMap = Arc<
    RwLock<
        std::collections::HashMap<
            String,
            (
                String,
                serde_json::Value,
                Arc<crate::cli::messages::WorkUnit>,
                usize,
            ),
        >,
    >,
>;

/// Refresh the ContextLine status-strip entries and the terminal window/tab title.
///
/// `context_lines` is the total number of lines to show including the 🧠 stats
/// line, so `depth = context_lines - 1` centroid lines are requested from the
/// MemTree.  Stale `ContextLine(N)` entries beyond the result are removed so
/// the strip shrinks cleanly when history is short.
///
/// This is a free function (not `&self`) so it can be called from the static
/// `process_query_with_tools` closure.
pub(super) async fn refresh_context_strip(
    memory_system: &crate::memory::MemorySystem,
    session_label: &str,
    cwd: &str,
    status_bar: &StatusBar,
    context_lines: usize,
) {
    let depth = context_lines.saturating_sub(1); // 🧠 takes one slot
    let Ok(summary) = memory_system.conversation_summary(depth).await else {
        return;
    };

    let n = summary.lines.len();

    // Format each line with an appropriate prefix:
    //   single line                → "   └─ now: <text>"
    //   first of multiple          → "📋 <text>"
    //   middle lines               → "   ├─ <text>"
    //   last of multiple           → "   └─ now: <text>"
    for (i, text) in summary.lines.iter().enumerate() {
        let label = if n == 1 {
            format!("   └─ now: {}", text)
        } else if i == 0 {
            format!("📋 {}", text)
        } else if i == n - 1 {
            format!("   └─ now: {}", text)
        } else {
            format!("   ├─ {}", text)
        };
        status_bar.update_line(
            crate::cli::status_bar::StatusLineType::ContextLine(i),
            label,
        );
    }

    // Remove stale slots beyond what we just wrote (depth change or short history)
    for i in n..8 {
        status_bar.remove_line(&crate::cli::status_bar::StatusLineType::ContextLine(i));
    }

    // OSC 0 — set terminal window title + tab title
    let title_topic = summary.lines.first().map(|s| {
        if s.chars().count() <= 35 {
            s.to_string()
        } else {
            format!("{}…", s.chars().take(34).collect::<String>())
        }
    });
    let title = match title_topic.as_deref() {
        Some(t) if !t.is_empty() => format!("finch · {} · {} · {}", session_label, cwd, t),
        _ => format!("finch · {} · {}", session_label, cwd),
    };
    {
        use std::io::Write as _;
        print!("\x1b]0;{}\x07", title);
        let _ = std::io::stdout().flush();
    }
}

/// Dispatch a batch of tool uses for one query turn.
///
/// Called from both the streaming and non-streaming response paths — they used
/// to each contain an identical 115-line block.  This function is the single
/// source of truth for:
///
/// * Loop detection (same tool+args called twice → terminal error)
/// * Plan-mode tool gating (blocks Write/Edit/Bash in Planning mode)
/// * WorkUnit row creation and `active_tool_uses` registration
/// * Inline dispatch for `AskUserQuestion` and `PresentPlan`
/// * Fallback to `ToolExecutionCoordinator::spawn_tool_execution`
/// * Memory status bar refresh after all tools are queued
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_tool_uses(
    tool_uses: Vec<crate::tools::types::ToolUse>,
    query_id: Uuid,
    work_unit: &Arc<crate::cli::messages::WorkUnit>,
    mode: &Arc<RwLock<ReplMode>>,
    tool_call_history: &Arc<
        RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>,
    >,
    event_tx: &mpsc::UnboundedSender<ReplEvent>,
    active_tool_uses: &ActiveToolUsesMap,
    tui_renderer: &Arc<tokio::sync::Mutex<crate::cli::tui::TuiRenderer>>,
    output_manager: &Arc<crate::cli::output_manager::OutputManager>,
    query_states: &Arc<super::query_state::QueryStateManager>,
    tool_coordinator: &super::tool_execution::ToolExecutionCoordinator,
    memory_system: &Option<Arc<crate::memory::MemorySystem>>,
    memory_recall_count: usize,
    session_label: &str,
    cwd: &str,
    status_bar: &Arc<crate::cli::StatusBar>,
    context_lines: usize,
) {
    use super::plan_handler::{handle_ask_user_question, handle_present_plan, is_tool_allowed_in_mode};
    use super::tool_display::format_tool_label;
    use tokio_util::sync::CancellationToken;

    let current_mode = mode.read().await;
    for tool_use in tool_uses {
        // Loop detection: a second identical (tool, input) call for this query means
        // the model is stuck; return a terminal error so it breaks out.
        let call_key = format!("{}:{}", tool_use.name, tool_use.input);
        let call_count = {
            let mut history = tool_call_history.write().await;
            let entry = history
                .entry(query_id)
                .or_insert_with(std::collections::HashMap::new);
            let count = entry.entry(call_key).or_insert(0);
            *count += 1;
            *count
        };
        if call_count > 1 {
            let label = format_tool_label(&tool_use.name, &tool_use.input);
            let row_idx = work_unit.add_row(label);
            work_unit.fail_row(row_idx, "loop detected");
            let error_msg = format!(
                "LOOP DETECTED: You have called {} with the same arguments {} time(s) and received the same result each time.\n\
                 Repeating this call will not produce different output.\n\
                 You have enough information to proceed. Call PresentPlan now to show your plan.",
                tool_use.name,
                call_count - 1
            );
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id: tool_use.id.clone(),
                result: Err(anyhow::anyhow!("{}", error_msg)),
            });
            continue;
        }

        // Plan-mode gate: block destructive tools while exploring
        if !is_tool_allowed_in_mode(&tool_use.name, &current_mode) {
            let label = format_tool_label(&tool_use.name, &tool_use.input);
            let row_idx = work_unit.add_row(label);
            work_unit.fail_row(row_idx, "blocked in plan mode");
            let error_msg = format!(
                "Tool '{}' is not allowed in planning mode.\n\
                 Reason: This tool can modify system state.\n\
                 Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
                 Type /approve to execute your plan with all tools enabled.",
                tool_use.name
            );
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id: tool_use.id.clone(),
                result: Err(anyhow::anyhow!("{}", error_msg)),
            });
            continue;
        }

        // Add a running row for this tool in the shared WorkUnit
        let label = format_tool_label(&tool_use.name, &tool_use.input);
        let row_idx = work_unit.add_row(&label);
        active_tool_uses.write().await.insert(
            tool_use.id.clone(),
            (tool_use.name.clone(), tool_use.input.clone(), Arc::clone(work_unit), row_idx),
        );

        // Inline handlers for interactive tools (block until dialog resolved)
        if let Some(result) =
            handle_ask_user_question(&tool_use, Arc::clone(tui_renderer)).await
        {
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id: tool_use.id.clone(),
                result,
            });
        } else if let Some(result) = handle_present_plan(
            &tool_use,
            Arc::clone(tui_renderer),
            Arc::clone(mode),
            Arc::clone(output_manager),
            query_states
                .get_metadata(query_id)
                .await
                .map(|m| m.cancellation_token)
                .unwrap_or_else(CancellationToken::new),
            Arc::clone(work_unit),
        )
        .await
        {
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id: tool_use.id.clone(),
                result,
            });
        } else {
            // Regular tool: run concurrently in a background task
            tool_coordinator.spawn_tool_execution(
                query_id,
                tool_use,
                Arc::clone(work_unit),
                row_idx,
            );
        }
    }
    drop(current_mode);

    // Update memory status bar now that tools are queued
    if let Some(ref mem) = memory_system {
        if let Ok(stats) = mem.stats().await {
            status_bar.update_line(
                crate::cli::status_bar::StatusLineType::MemoryContext,
                format!(
                    "🧠 recalled {}  ·  {} memories",
                    memory_recall_count, stats.conversation_count
                ),
            );
        }
        refresh_context_strip(mem, session_label, cwd, status_bar, context_lines).await;
    }
}

/// Process a query with potential tool execution loop using unified generators.
///
/// This is a free function (not a method) so it can be called from a
/// `tokio::spawn` closure in `EventLoop::spawn_query_task` without capturing
/// `self`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_query_with_tools(
    query_id: Uuid,
    query: String,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    claude_gen: Arc<dyn Generator>,
    qwen_gen: Arc<dyn Generator>,
    router: Arc<Router>,
    generator_state: Arc<RwLock<GeneratorState>>,
    tool_definitions: Arc<Vec<ToolDefinition>>,
    conversation: Arc<RwLock<ConversationHistory>>,
    query_states: Arc<QueryStateManager>,
    tool_coordinator: ToolExecutionCoordinator,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
    mode: Arc<RwLock<ReplMode>>,
    output_manager: Arc<OutputManager>,
    status_bar: Arc<crate::cli::StatusBar>,
    active_tool_uses: ActiveToolUsesMap,
    memory_system: Option<Arc<crate::memory::MemorySystem>>,
    session_label: String,
    cwd: String,
    context_lines: usize,
    max_verbatim: usize,
    recall_k: usize,
    enable_summarization: bool,
    auto_compact_enabled: bool,
    summary_gen: Arc<dyn Generator>,
    tool_call_history: Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,
) {
    tracing::debug!(
        "process_query_with_tools starting for query_id: {:?}",
        query_id
    );

    // Step 1: Routing decision
    let generator: Arc<dyn Generator> = {
        // Check if Qwen is ready
        let state = generator_state.read().await;
        let qwen_ready = state.is_ready();
        drop(state);

        // Route based on readiness and confidence
        // NOTE: In daemon mode, these logs are misleading (daemon makes actual routing decision)
        // TODO: Detect daemon mode and skip client-side routing entirely
        if qwen_ready {
            match router.route(&query) {
                crate::router::RouteDecision::Local { confidence, .. } if confidence > 0.7 => {
                    // Use Qwen
                    tracing::debug!(
                        "Client-side routing: Qwen (confidence: {:.2})",
                        confidence
                    );
                    Arc::clone(&qwen_gen)
                }
                _ => {
                    // Use Claude
                    tracing::debug!(
                        "Client-side routing: teacher (low confidence or no match)"
                    );
                    Arc::clone(&claude_gen)
                }
            }
        } else {
            // Qwen not ready, use Claude
            tracing::debug!("Client-side routing: teacher (Qwen not ready)");
            Arc::clone(&claude_gen)
        }
    };

    // Get conversation context, optionally injecting relevant memories
    let mut memory_recall_count: usize = 0;
    let messages = {
        let all_msgs = conversation.read().await.get_messages();
        // When summarization is enabled and messages have been dropped by the
        // sliding window, summarise them and inject as a prefix so the LLM
        // retains awareness of earlier turns.
        let mut msgs =
            if enable_summarization && max_verbatim > 0 && all_msgs.len() > max_verbatim {
                let drop_end = all_msgs.len() - max_verbatim;
                // Clone the dropped slice so we can pass all_msgs by value to apply_sliding_window.
                let dropped: Vec<_> = all_msgs[..drop_end].to_vec();
                let window = apply_sliding_window(all_msgs, max_verbatim);
                let compactor =
                    crate::cli::conversation_compactor::ConversationCompactor::new(summary_gen);
                compactor.compact(&dropped, window).await
            } else {
                apply_sliding_window(all_msgs, max_verbatim)
            };
        if let Some(ref mem) = memory_system {
            if let Ok(memories) = mem.query(&query, Some(recall_k)).await {
                if !memories.is_empty() {
                    memory_recall_count = memories.len();
                    let mem_block = memories.join("\n\n---\n\n");
                    // Inject into the last user message so the LLM sees the recalled context
                    if let Some(last_user) = msgs.iter_mut().rev().find(|m| m.role == "user") {
                        if let Some(ContentBlock::Text { ref mut text }) =
                            last_user.content.first_mut()
                        {
                            *text = format!(
                                "[Relevant memories from past sessions:\n\n{}]\n\n{}",
                                mem_block, text
                            );
                        }
                    }
                    status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::MemoryContext,
                        format!("🧠 recalled {}  ·  querying…", memory_recall_count),
                    );
                }
            }
        }
        msgs
    };
    let caps = generator.capabilities();

    // Try streaming first if supported
    if caps.supports_streaming {
        tracing::debug!("Generator supports streaming, attempting to stream");

        // Create a WorkUnit for this generation turn BEFORE streaming begins.
        // The shadow-buffer / insert_before architecture requires the message to
        // exist in output_manager before any blit cycles run — the WorkUnit's
        // time-driven animation will be visible during streaming.
        let verb = crate::cli::messages::random_spinner_verb();
        let work_unit = output_manager.start_work_unit(verb);

        let stream_start = std::time::Instant::now();
        let mut token_count: usize = 0;
        let mut input_token_count: Option<u32> = None;
        {
            use std::io::Write as _;
            print!(
                "\x1b]0;finch · {} · {} · ↓ streaming…\x07",
                session_label, cwd
            );
            let _ = std::io::stdout().flush();
        }

        match generator
            .generate_stream(messages.clone(), Some((*tool_definitions).clone()))
            .await
        {
            Ok(Some(mut rx)) => {
                tracing::debug!("[EVENT_LOOP] Streaming started, entering receive loop");
                tracing::debug!("Streaming started successfully");

                // Process stream (handles tools via StreamChunk::ContentBlockComplete)
                let mut blocks = Vec::new();
                let mut text = String::new();

                while let Some(result) = rx.recv().await {
                    match result {
                        Ok(StreamChunk::Usage { input_tokens }) => {
                            input_token_count = Some(input_tokens);
                        }
                        Ok(StreamChunk::TextDelta(delta)) => {
                            tracing::debug!("Received TextDelta: {} bytes", delta.len());
                            text.push_str(&delta);
                            token_count += delta.split_whitespace().count();
                            // WorkUnit accumulates tokens for its own animated display
                            work_unit.add_tokens(&delta);
                        }
                        Ok(StreamChunk::ContentBlockComplete(block)) => {
                            tracing::debug!("Received ContentBlockComplete: {:?}", block);
                            blocks.push(block);
                        }
                        Err(e) => {
                            tracing::error!("Stream error in event loop: {}", e);
                            work_unit.set_failed();
                            let _ = event_tx.send(ReplEvent::QueryFailed {
                                query_id,
                                error: format!("{}", e),
                            });
                            return;
                        }
                    }
                }

                tracing::debug!(
                    "[EVENT_LOOP] Stream receive loop ended, {} blocks received",
                    blocks.len()
                );
                tracing::debug!("Stream receive loop ended");

                // Stream complete — set the final response text on the WorkUnit.
                // If tools follow, set_complete() will be called after all tools finish.
                // If no tools, set_complete() is called below.
                if !text.is_empty() {
                    work_unit.set_response(&text);
                }

                // Send stats update
                let _ = event_tx.send(ReplEvent::StatsUpdate {
                    model: generator.name().to_string(),
                    input_tokens: input_token_count,
                    output_tokens: Some(token_count as u32),
                    latency_ms: Some(stream_start.elapsed().as_millis() as u64),
                });

                tracing::debug!("[EVENT_LOOP] Streaming complete");

                // Extract tools from blocks
                tracing::debug!("[EVENT_LOOP] Extracting tools from blocks");
                let tool_uses: Vec<ToolUse> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        }),
                        _ => None,
                    })
                    .collect();

                tracing::debug!("[EVENT_LOOP] Found {} tool uses", tool_uses.len());

                if !tool_uses.is_empty() {
                    tracing::debug!("[EVENT_LOOP] Tools detected, updating query state");
                    // Update state: executing tools
                    query_states
                        .update_state(
                            query_id,
                            QueryState::ExecutingTools {
                                tools_pending: tool_uses.len(),
                                tools_completed: 0,
                            },
                        )
                        .await;

                    tracing::debug!(
                        "[EVENT_LOOP] Query state updated, adding assistant message"
                    );
                    // Add assistant message with ALL content blocks (text + tool uses)
                    // This is critical for proper conversation structure
                    let assistant_message = crate::claude::Message {
                        role: "assistant".to_string(),
                        content: blocks.clone(),
                    };
                    tracing::debug!("[EVENT_LOOP] Acquiring conversation write lock...");
                    conversation.write().await.add_message(assistant_message);
                    tracing::debug!(
                        "[EVENT_LOOP] Assistant message added, spawning tool executions"
                    );

                    // Dispatch tools (loop detection, mode gating, inline handlers, spawn)
                    dispatch_tool_uses(
                        tool_uses,
                        query_id,
                        &work_unit,
                        &mode,
                        &tool_call_history,
                        &event_tx,
                        &active_tool_uses,
                        &tui_renderer,
                        &output_manager,
                        &query_states,
                        &tool_coordinator,
                        &memory_system,
                        memory_recall_count,
                        &session_label,
                        &cwd,
                        &status_bar,
                        context_lines,
                    )
                    .await;
                    tracing::debug!("[EVENT_LOOP] Tool executions spawned, returning");
                    return;
                }

                // No tools — mark WorkUnit complete so blit shows final response
                work_unit.set_complete();

                // Add assistant message to conversation
                tracing::debug!(
                    "[EVENT_LOOP] No tools found, adding assistant message to conversation"
                );
                conversation
                    .write()
                    .await
                    .add_assistant_message(text.clone());

                // Store to memory (fire-and-forget; never blocks the response path)
                if let Some(ref mem) = memory_system {
                    let model_name = generator.name().to_string();
                    let _ = mem
                        .insert_conversation(
                            "user",
                            &query,
                            Some(&model_name),
                            Some(&session_label),
                        )
                        .await;
                    let _ = mem
                        .insert_conversation(
                            "assistant",
                            &text,
                            Some(&model_name),
                            Some(&session_label),
                        )
                        .await;
                    if let Ok(stats) = mem.stats().await {
                        status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::MemoryContext,
                            format!(
                                "🧠 recalled {}  ·  {} memories",
                                memory_recall_count, stats.conversation_count
                            ),
                        );
                    }
                    refresh_context_strip(
                        mem,
                        &session_label,
                        &cwd,
                        &status_bar,
                        context_lines,
                    )
                    .await;
                }

                // Update context usage indicator (suppressed when auto-compact disabled)
                if auto_compact_enabled {
                    let conv = conversation.read().await;
                    let pct = (conv.compaction_percent_remaining() * 100.0) as u8;
                    status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::CompactionPercent,
                        format!("Context left until auto-compact: {}%", pct),
                    );
                }

                // Update query state
                query_states
                    .update_state(
                        query_id,
                        QueryState::Completed {
                            response: text.clone(),
                        },
                    )
                    .await;

                // Signal the event loop to clear active_query_id (streaming path never sends this otherwise)
                let _ = event_tx.send(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: text.clone(),
                });

                tracing::debug!("[EVENT_LOOP] Query complete, returning");
                return;
            }
            Ok(None) | Err(_) => {
                // Fall through to non-streaming
            }
        }
    }

    // Non-streaming path (for Qwen or fallback)
    // Create WorkUnit before the blocking generate call so the animated
    // header is visible during the wait (blit cycle runs every ~100ms).
    let verb = crate::cli::messages::random_spinner_verb();
    let work_unit = output_manager.start_work_unit(verb);
    match generator
        .generate(messages, Some((*tool_definitions).clone()))
        .await
    {
        Ok(response) => {
            // Set response text on the WorkUnit
            if !response.text.is_empty() {
                work_unit.set_response(&response.text);
            }

            // Send stats update
            let _ = event_tx.send(ReplEvent::StatsUpdate {
                model: response.metadata.model.clone(),
                input_tokens: response.metadata.input_tokens,
                output_tokens: response.metadata.output_tokens,
                latency_ms: response.metadata.latency_ms,
            });

            // Send response (StreamingComplete works for non-streaming too)
            let _ = event_tx.send(ReplEvent::StreamingComplete {
                query_id,
                full_response: response.text.clone(),
            });

            // Convert GenToolUse to ToolUse
            let tool_uses: Vec<ToolUse> = response
                .tool_uses
                .into_iter()
                .map(|gen_tool| ToolUse {
                    id: gen_tool.id,
                    name: gen_tool.name,
                    input: gen_tool.input,
                })
                .collect();

            if !tool_uses.is_empty() {
                // Update state: executing tools
                query_states
                    .update_state(
                        query_id,
                        QueryState::ExecutingTools {
                            tools_pending: tool_uses.len(),
                            tools_completed: 0,
                        },
                    )
                    .await;

                // Add assistant message with ALL content blocks (text + tool uses)
                // This is critical for proper conversation structure
                let assistant_message = crate::claude::Message {
                    role: "assistant".to_string(),
                    content: response.content_blocks.clone(),
                };
                conversation.write().await.add_message(assistant_message);

                // Dispatch tools (loop detection, mode gating, inline handlers, spawn)
                dispatch_tool_uses(
                    tool_uses,
                    query_id,
                    &work_unit,
                    &mode,
                    &tool_call_history,
                    &event_tx,
                    &active_tool_uses,
                    &tui_renderer,
                    &output_manager,
                    &query_states,
                    &tool_coordinator,
                    &memory_system,
                    memory_recall_count,
                    &session_label,
                    &cwd,
                    &status_bar,
                    context_lines,
                )
                .await;
                return;
            }

            // No tools — mark WorkUnit complete
            work_unit.set_complete();
            tracing::debug!("Query complete (no tools), non-streaming finished");

            // Store to memory (fire-and-forget)
            if let Some(ref mem) = memory_system {
                let model_name = response.metadata.model.clone();
                let _ = mem
                    .insert_conversation(
                        "user",
                        &query,
                        Some(&model_name),
                        Some(&session_label),
                    )
                    .await;
                let _ = mem
                    .insert_conversation(
                        "assistant",
                        &response.text,
                        Some(&model_name),
                        Some(&session_label),
                    )
                    .await;
                if let Ok(stats) = mem.stats().await {
                    status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::MemoryContext,
                        format!(
                            "🧠 recalled {}  ·  {} memories",
                            memory_recall_count, stats.conversation_count
                        ),
                    );
                }
                refresh_context_strip(mem, &session_label, &cwd, &status_bar, context_lines)
                    .await;
            }
        }
        Err(e) => {
            let _ = event_tx.send(ReplEvent::QueryFailed {
                query_id,
                error: format!("{}", e),
            });
        }
    }
}

/// Apply a sliding window to the message list, keeping only the last `max` messages
/// verbatim. If `max` is 0 or the list is shorter than `max`, returns all messages.
///
/// After slicing, advances past any leading assistant messages so the window
/// always starts with a user turn (required by all provider APIs). Also strips
/// any leading user messages that contain only `tool_result` blocks — these are
/// orphaned when the sliding window cuts the preceding assistant `tool_use`
/// message, and all providers reject `tool_result` without a matching `tool_use`.
/// A floor of 2 messages is kept to avoid sending an empty window in degenerate
/// cases.
pub(crate) fn apply_sliding_window(
    msgs: Vec<crate::claude::Message>,
    max: usize,
) -> Vec<crate::claude::Message> {
    if max == 0 || msgs.len() <= max {
        return msgs;
    }
    let mut window = msgs[msgs.len() - max..].to_vec();
    // Ensure the window starts with a user message (API requirement).
    while window.len() > 2 && window.first().map(|m| m.role.as_str()) == Some("assistant") {
        window.remove(0);
    }
    // Strip orphaned tool_result-only user messages at the window boundary.
    // This happens when the cut falls inside a tool-call round-trip: the
    // assistant tool_use was dropped but the user tool_result survived.
    // Every provider rejects tool_result blocks without a matching tool_use.
    loop {
        if window.len() <= 2 {
            break;
        }
        let first_is_orphaned = window.first().map(|m| {
            m.role == "user"
                && !m.content.is_empty()
                && m.content
                    .iter()
                    .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        if first_is_orphaned != Some(true) {
            break;
        }
        window.remove(0); // drop orphaned tool_result user turn
        // Also drop the assistant reply that followed it (starts the next pair).
        if window.first().map(|m| m.role.as_str()) == Some("assistant") {
            window.remove(0);
        }
    }
    window
}
