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
use crate::cli::output_manager::{OutputManager, VmOutputProjection};
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::generators::{Generator, StreamChunk};
use crate::models::bootstrap::GeneratorState;
use crate::router::Router;
use crate::tools::types::{ToolDefinition, ToolUse};

/// Preserve a provider response as submitted wire source.
///
/// Markdown fences are deliberately *not* unwrapped. The wire protocol makes
/// them malformed input so the model receives a structured correction instead
/// of silently learning that an undocumented wrapper is accepted. Trimming
/// only outer framing whitespace never changes literal contents.
fn raw_wire_source(source: &str) -> String {
    source.trim().to_string()
}

/// Build the submission for a provider response carried on the VM wire rather
/// than in a provider-native tool call.  The typed runtime derives authority
/// from the program itself; `Pure` is only the coarse compatibility label and
/// does not bypass typed capability checks.
fn direct_wire_submission(
    runtime: &crate::runtime::ProgramRuntime,
    source: String,
) -> anyhow::Result<crate::runtime::ProgramSubmission> {
    let language = crate::programs::ProgramLanguage::infer_wire_source(&source)?;
    Ok(crate::runtime::ProgramSubmission {
        language,
        source,
        intent: "provider VM-wire response".to_string(),
        effect: crate::programs::ExecutionEffect::Pure,
        declared_capabilities: Vec::new(),
        manifest_generation: runtime.manifest_generation(),
        expected_revision: Some(runtime.revision()),
        budget: None,
    })
}

/// Execute a completed provider text response as Finch source.  This is the
/// actual wire receiver: raw model text is no longer treated as prose merely
/// because it did not arrive in a provider-native tool-call envelope.
async fn execute_direct_wire_response(
    runtime: &crate::runtime::ProgramRuntime,
    output_manager: Arc<OutputManager>,
    work_unit: Arc<crate::cli::messages::WorkUnit>,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    source: String,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    let submission = direct_wire_submission(runtime, source)?;
    let projection = VmOutputProjection::new(output_manager, work_unit);
    let sink: crate::runtime::TypedEffectSink = Arc::new(move |envelope| {
        // The typed VM executes on a blocking worker. Projection belongs on
        // the event-loop task so shadow-buffer mutations and rendering remain
        // serialized with all other client events.
        let _ = event_tx.send(ReplEvent::VmEffect {
            projection: projection.clone(),
            envelope,
        });
    });
    runtime
        .submit_with_deferred_program_effects(submission, sink)
        .await
}

/// A rejected provider response may be repaired once only when the VM proved
/// that it never began an external operation.  In particular, an approval,
/// suspension, cancellation, timeout, or journaled host effect is an execution
/// boundary rather than a syntax-editing opportunity.
fn is_repairable_wire_outcome(outcome: &crate::runtime::outcome::ExecutionOutcome) -> bool {
    use crate::runtime::outcome::ExecutionStatus;

    if outcome.status != ExecutionStatus::Failed
        || !outcome.side_effects.is_empty()
        || !outcome.vm_side_effects.is_empty()
        || !outcome.effect_journal.is_empty()
    {
        return false;
    }

    outcome
        .vm_diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .chain(outcome.diagnostics.iter().map(String::as_str))
        .any(is_repairable_wire_diagnostic)
}

fn is_repairable_wire_diagnostic(diagnostic: &str) -> bool {
    matches!(
        diagnostic,
        value if value.starts_with("E-READ-")
            || value.starts_with("E-TYPE-")
            || value.starts_with("E-STACK-")
            || value.starts_with("E-LISP-")
            || value.starts_with("E-FORTH-")
            || value.starts_with("E-LINK-")
            || value.starts_with("E-CAP-")
            || value.starts_with("E-WIRE-")
    )
}

fn wire_repair_messages(
    messages: &[crate::claude::Message],
    rejected_source: &str,
    diagnostic: &str,
) -> Vec<crate::claude::Message> {
    let mut repair_messages = messages.to_vec();
    repair_messages.push(crate::claude::Message::assistant(rejected_source));
    repair_messages.push(crate::claude::Message::user(format!(
        "The preceding Finch VM wire program was rejected before execution. \
         Re-emit exactly one complete raw Finch Lisp or Co-Forth program; do not use Markdown, prose, or tools.\n\n\
         Rejected source:\n---\n{rejected_source}\n---\n\
         Diagnostic:\n{diagnostic}"
    )));
    repair_messages
}

struct WireExecution {
    source_for_history: String,
    response: String,
}

/// Execute one provider wire response and, for a safe rejected program, ask
/// the same model for precisely one source-level correction.  Each source and
/// each output owns a separate WorkUnit, so the failed program never vanishes
/// from scrollback when the replacement succeeds.
async fn execute_wire_with_single_repair(
    runtime: &crate::runtime::ProgramRuntime,
    output_manager: Arc<OutputManager>,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    generator: Arc<dyn Generator>,
    messages: &[crate::claude::Message],
    source: String,
) -> WireExecution {
    let output_unit = output_manager.start_work_unit("VM program output");
    output_unit.set_program_output();
    let initial = execute_direct_wire_response(
        runtime,
        Arc::clone(&output_manager),
        Arc::clone(&output_unit),
        event_tx.clone(),
        source.clone(),
    )
    .await;

    let (diagnostic, repairable) = match initial {
        Ok(outcome) if outcome.status == crate::runtime::outcome::ExecutionStatus::Completed => {
            output_unit.set_complete();
            return WireExecution {
                source_for_history: source,
                response: outcome.output,
            };
        }
        Ok(outcome) => {
            let detail = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            (detail, is_repairable_wire_outcome(&outcome))
        }
        Err(error) => {
            let detail = error.to_string();
            (detail.clone(), is_repairable_wire_diagnostic(&detail))
        }
    };

    output_unit.append_response(&format!("VM wire error: {diagnostic}"));
    if !repairable {
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
        };
    }

    let repair_messages = wire_repair_messages(messages, &source, &diagnostic);
    let repair = generator.generate(repair_messages, None).await;
    let Ok(repair) = repair else {
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
        };
    };
    if !repair.tool_uses.is_empty() || repair.text.trim().is_empty() {
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
        };
    }
    output_unit.set_complete();

    let repaired_source = raw_wire_source(&repair.text);
    let repair_source_unit = output_manager.start_work_unit("VM program repair");
    repair_source_unit.set_program_source(
        crate::programs::ProgramLanguage::infer_source(&repaired_source).as_str(),
    );
    repair_source_unit.set_response(repaired_source.clone());
    repair_source_unit.set_complete();

    let repair_output_unit = output_manager.start_work_unit("VM repaired program output");
    repair_output_unit.set_program_output();
    match execute_direct_wire_response(
        runtime,
        output_manager,
        Arc::clone(&repair_output_unit),
        event_tx,
        repaired_source.clone(),
    )
    .await
    {
        Ok(outcome) if outcome.status == crate::runtime::outcome::ExecutionStatus::Completed => {
            repair_output_unit.set_complete();
            WireExecution {
                source_for_history: repaired_source,
                response: outcome.output,
            }
        }
        Ok(outcome) => {
            let detail = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            repair_output_unit.append_response(&format!("VM wire error: {detail}"));
            repair_output_unit.set_complete();
            WireExecution {
                source_for_history: repaired_source,
                response: detail,
            }
        }
        Err(error) => {
            let detail = format!("VM wire error: {error}");
            repair_output_unit.append_response(&detail);
            repair_output_unit.set_complete();
            WireExecution {
                source_for_history: repaired_source,
                response: detail,
            }
        }
    }
}

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
    let Ok(summary) = memory_system
        .conversation_summary_for_session(session_label, depth)
        .await
    else {
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
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(title));
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
    use super::plan_handler::{
        handle_ask_user_question, handle_present_plan, is_tool_allowed_in_mode,
    };
    use super::tool_display::format_tool_label;
    use tokio_util::sync::CancellationToken;

    let current_mode = mode.read().await;
    for tool_use in tool_uses {
        // Loop detection: a second identical (tool, input) call for this query means
        // the model is stuck; return a terminal error so it breaks out.
        //
        // Skip detection for no-argument tools (empty JSON object input).  These
        // tools — Run, Clear, View — are intentionally stateless; calling them
        // twice is meaningful (e.g. signalling readiness, then confirming after
        // the user interacted), so there is nothing to deduplicate.
        let input_is_empty = tool_use.input == serde_json::json!({});
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
        if !input_is_empty && call_count > 1 {
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
                 Available tools: read, glob, grep, web_fetch, todo_read, todo_write, present_plan, ask_user_question\n\
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
            (
                tool_use.name.clone(),
                tool_use.input.clone(),
                Arc::clone(work_unit),
                row_idx,
            ),
        );

        // Inline handlers for interactive tools (block until dialog resolved)
        if let Some(result) = handle_ask_user_question(
            &tool_use,
            Arc::clone(tui_renderer),
            query_states
                .get_metadata(query_id)
                .await
                .map(|m| m.cancellation_token)
                .unwrap_or_else(CancellationToken::new),
            event_tx,
        )
        .await
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
            event_tx,
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
    program_runtime: Arc<crate::runtime::ProgramRuntime>,
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
    tool_call_history: Arc<
        RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>,
    >,
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
                    tracing::debug!("Client-side routing: Qwen (confidence: {:.2})", confidence);
                    Arc::clone(&qwen_gen)
                }
                _ => {
                    // Use Claude
                    tracing::debug!("Client-side routing: teacher (low confidence or no match)");
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
        let mut msgs = if enable_summarization && max_verbatim > 0 && all_msgs.len() > max_verbatim
        {
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
                        format!("🧠 recalled {}", memory_recall_count),
                    );
                }
            }
        }
        // The bootstrap language contract must be present regardless of
        // whether MemTree is configured. When it is available, its program
        // registry enriches the same manifest with relevant vocabulary.
        if !query.is_empty() {
            let manifest = match memory_system.as_ref() {
                Some(memory) => memory
                    .vm_manifest(&query, 12)
                    .await
                    .unwrap_or_else(|_| fallback_vm_manifest()),
                None => fallback_vm_manifest(),
            };
            inject_vm_manifest(&mut msgs, &manifest);
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
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::terminal::SetTitle(format!(
                    "finch · {} · {} · ↓ streaming…",
                    session_label, cwd
                ))
            );
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
                            // The streamed text is VM source, not provisional
                            // assistant prose. As soon as its language can be
                            // identified, project the partial program visibly
                            // instead of hiding it behind the generic spinner.
                            if !text.trim_start().is_empty() {
                                let language =
                                    crate::programs::ProgramLanguage::infer_source(&text);
                                work_unit.set_program_source(language.as_str());
                            }
                            // Keep the shadow-buffer preview live. The program is
                            // still only parsed/executed at the explicit boundary.
                            work_unit.set_response(&text);
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

                    tracing::debug!("[EVENT_LOOP] Query state updated, adding assistant message");
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

                // A text-only provider response is Finch source, not prose.
                // Preserve the received program as one completed work unit,
                // then route its `say`/UI events to a distinct output unit.
                // This keeps agent activity inspectable without making source
                // and user-visible output compete for the same mutable row.
                let wire_source = raw_wire_source(&text);
                let wire_language = crate::programs::ProgramLanguage::infer_source(&wire_source);
                work_unit.set_program_source(wire_language.as_str());
                work_unit.set_response(wire_source.clone());
                work_unit.set_complete();
                let wire_execution = execute_wire_with_single_repair(
                    program_runtime.as_ref(),
                    Arc::clone(&output_manager),
                    event_tx.clone(),
                    Arc::clone(&generator),
                    &messages,
                    wire_source.clone(),
                )
                .await;
                let response = wire_execution.response;
                conversation
                    .write()
                    .await
                    .add_assistant_message(wire_execution.source_for_history);
                query_states
                    .update_state(
                        query_id,
                        QueryState::Completed {
                            response: response.clone(),
                        },
                    )
                    .await;
                let _ = event_tx.send(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: response,
                });
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
        .generate(messages.clone(), Some((*tool_definitions).clone()))
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

            // Non-streaming providers receive the same two-unit projection:
            // source first, then the independently reactive program output.
            let wire_source = raw_wire_source(&response.text);
            let wire_language = crate::programs::ProgramLanguage::infer_source(&wire_source);
            work_unit.set_program_source(wire_language.as_str());
            work_unit.set_response(wire_source.clone());
            work_unit.set_complete();
            let wire_execution = execute_wire_with_single_repair(
                program_runtime.as_ref(),
                Arc::clone(&output_manager),
                event_tx.clone(),
                Arc::clone(&generator),
                &messages,
                wire_source.clone(),
            )
            .await;
            let rendered_response = wire_execution.response;
            conversation
                .write()
                .await
                .add_assistant_message(wire_execution.source_for_history);
            query_states
                .update_state(
                    query_id,
                    QueryState::Completed {
                        response: rendered_response.clone(),
                    },
                )
                .await;
            let _ = event_tx.send(ReplEvent::StreamingComplete {
                query_id,
                full_response: rendered_response,
            });
            tracing::debug!("Query complete (no tools), non-streaming finished");

            // Store to memory (fire-and-forget)
            if let Some(ref mem) = memory_system {
                let model_name = response.metadata.model.clone();
                let query_for_memory = query
                    .split_once("\n\n[Context:")
                    .map(|(raw, _)| raw)
                    .unwrap_or(&query);
                let _ = mem
                    .insert_conversation(
                        "user",
                        query_for_memory,
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
                refresh_context_strip(mem, &session_label, &cwd, &status_bar, context_lines).await;
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

fn inject_vm_manifest(
    messages: &mut Vec<crate::claude::Message>,
    manifest: &crate::programs::VmManifest,
) -> bool {
    let protocol = manifest.prompt_block();
    let section = format!("## Finch VM wire protocol\n{protocol}");

    // The response shape is an execution contract, not user-provided context.
    // Keep it in the existing system instruction whenever the provider request
    // has one. Prepending it to every user turn made the contract easy for a
    // model to treat as ordinary quoted context (and was notably fragile for
    // models that already have strong chat-format priors).
    if let Some(system) = messages.iter_mut().find(|message| message.role == "system") {
        if let Some(ContentBlock::Text { text }) = system
            .content
            .iter_mut()
            .find(|block| matches!(block, ContentBlock::Text { .. }))
        {
            *text = format!("{text}\n\n{section}");
            return true;
        }
        system.content.push(ContentBlock::Text { text: section });
        return true;
    }

    // Providers without a preexisting persona still receive a real system
    // message. Do not fall back to smuggling the protocol into the user turn.
    messages.insert(
        0,
        crate::claude::Message {
            role: "system".to_string(),
            content: vec![ContentBlock::Text { text: section }],
        },
    );
    true
}

fn fallback_vm_manifest() -> crate::programs::VmManifest {
    crate::programs::VmManifest {
        protocol_version: crate::programs::MANIFEST_PROTOCOL_VERSION,
        registry_generation: 0,
        environment_hash: "unavailable".to_string(),
        languages: vec![
            crate::programs::ProgramLanguage::Forth,
            crate::programs::ProgramLanguage::Lisp,
        ],
        language_packages: crate::programs::language_package_identities(),
        core_effects: vec!["session.emit".to_string(), "vm.read".to_string()],
        relevant_programs: Vec::new(),
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
///
/// When the orphaned turn is followed immediately by another assistant `tool_use`
/// (the start of the next still-valid round-trip), a placeholder user message is
/// inserted instead of cascading removal. Without this, removing the orphan and
/// the following assistant would orphan the *next* tool_result, cascading all the
/// way down to a 2-message floor that still starts with an orphaned tool_result.
pub(crate) fn apply_sliding_window(
    msgs: Vec<crate::claude::Message>,
    max: usize,
) -> Vec<crate::claude::Message> {
    let mut window = if max == 0 || msgs.len() <= max {
        msgs
    } else {
        msgs[msgs.len() - max..].to_vec()
    };
    // Ensure the window starts with a user message (API requirement).
    while window.len() > 2 && window.first().map(|m| m.role.as_str()) == Some("assistant") {
        window.remove(0);
    }
    // Strip the orphaned tool_result-only user message at the window boundary.
    // This happens when the cut falls inside a tool-call round-trip: the
    // assistant tool_use was dropped but the user tool_result survived.
    // Every provider rejects tool_result without a matching preceding tool_use.
    //
    // After removing the orphan, the window may start with the *next* assistant
    // tool_use (which has a valid paired user:tool_result after it).  Rather than
    // cascading removal — which destroys those valid round-trips and ultimately
    // leaves another orphan at the 2-message floor — we insert a lightweight
    // placeholder user turn so the valid tool chain is preserved.
    loop {
        if window.is_empty() {
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
                          // If the window now starts with an assistant message (the next tool round),
                          // insert a placeholder user turn to satisfy the user-first invariant without
                          // cascading removal that would orphan every subsequent tool_result.
        if window.first().map(|m| m.role.as_str()) == Some("assistant") {
            window.insert(
                0,
                crate::claude::Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "[earlier context omitted by sliding window]".to_string(),
                    }],
                },
            );
            break;
        }
    }
    // Final pass: strip any assistant messages that have tool_use blocks but are
    // not immediately followed by a user message with ALL matching tool_results.
    // This handles conversations corrupted by cancelled queries: the assistant
    // message with tool_uses was written to history but tool execution was aborted
    // before finalize_tool_execution could add the corresponding tool_result message.
    {
        use std::collections::HashSet;
        let mut i = 0;
        while i < window.len() {
            let tool_use_ids: Vec<String> = window[i]
                .content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, .. } = b {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            if tool_use_ids.is_empty() {
                i += 1;
                continue;
            }

            // Check the immediately following message for ALL matching tool_results.
            let next_covers_all = (i + 1 < window.len()).then(|| {
                let result_ids: HashSet<&str> = window[i + 1]
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
                tool_use_ids
                    .iter()
                    .all(|id| result_ids.contains(id.as_str()))
            });

            if next_covers_all == Some(true) {
                i += 1;
                continue;
            }

            // Orphaned tool_use found. Keep any text content; strip tool_use blocks.
            let text_blocks: Vec<ContentBlock> = window[i]
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Text { .. }))
                .cloned()
                .collect();

            if text_blocks.is_empty() {
                window.remove(i);
            } else {
                window[i] = crate::claude::Message {
                    role: "assistant".to_string(),
                    content: text_blocks,
                };
                i += 1;
            }

            // If the next message (now at index i) contains only tool_results they
            // are also orphaned — remove them so no tool_result arrives without a
            // preceding tool_use.
            if i < window.len()
                && !window[i].content.is_empty()
                && window[i]
                    .content
                    .iter()
                    .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                window.remove(i);
            }
        }
    }
    window
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SingleRepairGenerator {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Generator for SingleRepairGenerator {
        async fn generate(
            &self,
            messages: Vec<crate::claude::Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> anyhow::Result<crate::generators::GeneratorResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let repair_request = messages
                .last()
                .and_then(|message| message.content.first())
                .and_then(ContentBlock::as_text)
                .expect("wire repair must carry a corrective user message");
            assert!(repair_request.contains("E-WIRE-002"));
            Ok(crate::generators::GeneratorResponse {
                text: "(say \"repaired\")".to_string(),
                content_blocks: vec![ContentBlock::text("(say \"repaired\")")],
                tool_uses: Vec::new(),
                metadata: crate::generators::ResponseMetadata {
                    generator: "test".to_string(),
                    model: "test".to_string(),
                    confidence: None,
                    stop_reason: None,
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: None,
                },
            })
        }

        async fn generate_stream(
            &self,
            _messages: Vec<crate::claude::Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> anyhow::Result<Option<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>>>
        {
            Ok(None)
        }

        fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
            static CAPABILITIES: crate::generators::GeneratorCapabilities =
                crate::generators::GeneratorCapabilities {
                    supports_streaming: false,
                    supports_tools: false,
                    supports_conversation: true,
                    max_context_messages: Some(8),
                };
            &CAPABILITIES
        }

        fn name(&self) -> &str {
            "single-repair"
        }
    }

    #[test]
    fn fallback_manifest_injects_the_vm_bootstrap_without_memtree() {
        let mut messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "add two numbers".to_string(),
            }],
        }];

        assert!(inject_vm_manifest(&mut messages, &fallback_vm_manifest()));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        let ContentBlock::Text { text } = &messages[0].content[0] else {
            panic!("the system message must retain its text block");
        };
        assert!(text.contains("FINCH-VM-TYPED/1"));
        assert!(text.contains("## Mandatory response shape"));
        assert!(text.contains("Default to Lisp"));
        assert!(text.contains("otherwise treats the source as Forth"));
        assert!(text.contains("s\"response\" say"));
        assert!(!text.contains(".\" response\""));
        assert!(messages[1].content[0].as_text().is_some_and(|text| text == "add two numbers"));
    }

    #[test]
    fn manifest_joins_an_existing_system_instruction_not_the_user_turn() {
        let mut messages = vec![
            crate::claude::Message {
                role: "system".to_string(),
                content: vec![ContentBlock::Text {
                    text: "You are Finch's coding assistant.".to_string(),
                }],
            },
            crate::claude::Message::user("say hello"),
        ];

        assert!(inject_vm_manifest(&mut messages, &fallback_vm_manifest()));
        assert_eq!(messages.len(), 2);
        let system = messages[0].content[0].as_text().unwrap();
        assert!(system.starts_with("You are Finch's coding assistant."));
        assert!(system.contains("## Finch VM wire protocol"));
        assert!(system.contains("FINCH-VM-TYPED/1"));
        assert_eq!(messages[1].content[0].as_text(), Some("say hello"));
    }

    #[tokio::test]
    async fn direct_wire_text_is_a_lisp_or_forth_submission_not_display_prose() {
        let runtime = crate::runtime::ProgramRuntime::new();

        let lisp = direct_wire_submission(&runtime, "(say \"hello\")".to_string()).unwrap();
        assert_eq!(lisp.language, crate::programs::ProgramLanguage::Lisp);
        let outcome = runtime.submit_typed_only(lisp).await.unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.output, "hello");

        let forth = direct_wire_submission(&runtime, "s\"world\" say".to_string()).unwrap();
        assert_eq!(forth.language, crate::programs::ProgramLanguage::Forth);
        let outcome = runtime.submit_typed_only(forth).await.unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.output, "world");
    }

    #[test]
    fn preserves_fenced_source_for_a_structured_wire_diagnostic() {
        assert_eq!(
            raw_wire_source("```lisp\n(say \"hello\")\n```"),
            "```lisp\n(say \"hello\")\n```"
        );
        let runtime = crate::runtime::ProgramRuntime::new();
        let error =
            direct_wire_submission(&runtime, raw_wire_source("```forth\ns\"hello\" say\n```"))
                .unwrap_err();
        assert!(error.to_string().contains("E-WIRE-002"));
    }

    #[tokio::test]
    async fn fenced_wire_response_is_repaired_once_without_executing_its_body() {
        let runtime = crate::runtime::ProgramRuntime::new();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let generator = Arc::new(SingleRepairGenerator {
            calls: AtomicUsize::new(0),
        });
        let source = raw_wire_source("```lisp\n(say \"must not run\")\n```");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let execution = execute_wire_with_single_repair(
            &runtime,
            Arc::clone(&output),
            event_tx,
            generator.clone(),
            &[crate::claude::Message::user("reply")],
            source.clone(),
        )
        .await;

        // The worker only emits portable effects. The client event loop owns
        // the WorkUnit mutation, so apply the queued projection exactly as it
        // would on the live REPL task.
        let mut projected = 0;
        while let Ok(ReplEvent::VmEffect {
            projection,
            envelope,
        }) = event_rx.try_recv()
        {
            if projection.project_envelope(&envelope) {
                projected += 1;
            }
        }

        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(projected, 1, "the repaired say must cross the event bus");
        assert_eq!(execution.source_for_history, "(say \"repaired\")");
        assert_eq!(execution.response, "repaired");
        let messages = output.get_messages();
        assert_eq!(
            messages.len(),
            3,
            "source, failed output, repaired source/output"
        );
        assert!(messages.iter().all(|message| !message
            .format(&crate::config::ColorScheme::default())
            .contains("must not run")));
    }

    #[test]
    fn wire_repair_prompt_preserves_the_rejected_program_and_requires_raw_source() {
        let messages = vec![crate::claude::Message::user("say hello")];
        let repair = wire_repair_messages(&messages, "Hello!", "E-LINK-002: unknown word");
        assert_eq!(repair.len(), 3);
        assert_eq!(repair[1].role, "assistant");
        assert_eq!(repair[1].content[0].as_text(), Some("Hello!"));
        let prompt = repair[2].content[0].as_text().unwrap();
        assert!(prompt.contains("exactly one complete raw Finch Lisp or Co-Forth program"));
        assert!(prompt.contains("Hello!"));
        assert!(prompt.contains("E-LINK-002"));
    }

    #[test]
    fn wire_repair_classifier_excludes_runtime_and_external_boundaries() {
        assert!(is_repairable_wire_diagnostic(
            "E-READ-004: unterminated string"
        ));
        assert!(is_repairable_wire_diagnostic("E-LINK-002: unknown word"));
        assert!(is_repairable_wire_diagnostic(
            "E-WIRE-002: Markdown code fence"
        ));
        assert!(!is_repairable_wire_diagnostic(
            "E-LIMIT-001: fuel exhausted"
        ));

        let mut outcome = crate::runtime::outcome::ExecutionOutcome::failed(
            Uuid::nil(),
            0,
            crate::programs::ExecutionEffect::Pure,
            crate::runtime::outcome::ExecutionBackend::TypedVm,
            "E-TYPE-002: expected int",
            0,
        );
        assert!(is_repairable_wire_outcome(&outcome));
        outcome
            .side_effects
            .push(crate::vm::interpreter::HostSideEffect::Emit {
                text: "partial".into(),
            });
        assert!(!is_repairable_wire_outcome(&outcome));
    }
}
