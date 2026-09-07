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

fn history_content_with_source(
    blocks: &[ContentBlock],
    source: String,
) -> anyhow::Result<Vec<ContentBlock>> {
    let text_count = blocks.iter().filter(|block| block.is_text()).count();
    if text_count > 1 {
        let original = blocks
            .iter()
            .filter_map(ContentBlock::as_text)
            .collect::<String>();
        anyhow::ensure!(
            original == source,
            "Provider continuation with multiple text items cannot be rewritten without reordering opaque content"
        );
        return Ok(blocks.to_vec());
    }
    let mut inserted_text = false;
    let mut content = Vec::with_capacity(blocks.len().max(1));
    for block in blocks {
        if matches!(block, ContentBlock::Text { .. }) {
            if !inserted_text {
                content.push(ContentBlock::Text {
                    text: source.clone(),
                });
                inserted_text = true;
            }
        } else {
            content.push(block.clone());
        }
    }
    if !inserted_text && !source.is_empty() {
        content.push(ContentBlock::Text { text: source });
    }
    Ok(content)
}

/// Whether a provider has emitted any candidate ProgramSubmission source.
///
/// The text channel is the Finch wire, so duplicating a small subset of the
/// Co-Forth lexer here would hide valid programs beginning with bare strings,
/// collections, quotations, negative numbers, or user-defined words. Invalid
/// source also remains visible until the complete-program boundary diagnoses
/// it, which makes provider protocol failures inspectable.
fn has_streamed_wire_source(source: &str) -> bool {
    !source.trim_start().is_empty()
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
        source_id: Some(format!("provider-response.{}", language.as_str())),
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
    cancel: tokio_util::sync::CancellationToken,
    source: String,
    effect_audit: Option<crate::server::RunnerEffectAuditControl>,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    let submission = direct_wire_submission(runtime, source)?;
    anyhow::ensure!(
        !cancel.is_cancelled(),
        "VM program cancelled before audited submission"
    );
    let projection = VmOutputProjection::new(output_manager, work_unit);
    let effect_event_tx = event_tx.clone();
    let sink: crate::runtime::TypedEffectSink = Arc::new(move |envelope| {
        // The typed VM executes on a blocking worker. Projection belongs on
        // the event-loop task so shadow-buffer mutations and rendering remain
        // serialized with all other client events.
        let _ = effect_event_tx.send(ReplEvent::VmEffect {
            projection: projection.clone(),
            envelope,
        });
    });
    let outcome = runtime
        .submit_tool_program(submission, None, Some(sink), true, effect_audit)
        .await?;
    let execution_id = outcome.execution_id;
    let mut resumed = Box::pin(resume_interactive_boundaries(runtime, event_tx, outcome));
    tokio::select! {
        biased;
        result = &mut resumed => result,
        _ = cancel.cancelled() => {
            match runtime.cancel_typed_execution_with_outcome(execution_id)? {
                Some(cancelled) => Ok(cancelled),
                // Completion crossed the runtime boundary before cancellation
                // acquired the retained continuation. Preserve that result.
                None => resumed.await,
            }
        }
    }
}

/// Drive only application-owned interactive boundaries. Cooperative yields
/// resume automatically; capability requests wait for a structured dialog
/// choice and then resume the exact retained continuation. Source is never
/// resubmitted, so approval cannot duplicate prior output or host effects.
pub(super) async fn resume_interactive_boundaries(
    runtime: &crate::runtime::ProgramRuntime,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    mut outcome: crate::runtime::outcome::ExecutionOutcome,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    loop {
        outcome = resume_interactive_yields(runtime, outcome).await?;
        let Some(prompt) = outcome.approval_prompts.first().cloned() else {
            return Ok(outcome);
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        event_tx
            .send(ReplEvent::VmApprovalNeeded {
                prompt: prompt.clone(),
                response_tx,
            })
            .map_err(|_| anyhow::anyhow!("interactive VM approval UI is unavailable"))?;
        let choice = response_rx
            .await
            .map_err(|_| anyhow::anyhow!("interactive VM approval dialog was cancelled"))?;
        outcome = runtime
            .resolve_typed_approval(&prompt, choice, "interactive-user")
            .await?;
    }
}

/// Advance cooperative scheduling points without ever asking a human for new
/// authority. Scheduled and other unattended ProgramRuns fail closed at the
/// first approval boundary; the denial remains in the effect journal.
pub(super) async fn resume_noninteractive_boundaries(
    runtime: &crate::runtime::ProgramRuntime,
    mut outcome: crate::runtime::outcome::ExecutionOutcome,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    loop {
        outcome = resume_interactive_yields(runtime, outcome).await?;
        let Some(prompt) = outcome.approval_prompts.first().cloned() else {
            return Ok(outcome);
        };
        outcome = runtime
            .resolve_typed_approval(
                &prompt,
                crate::vm::ApprovalChoice::Deny,
                "noninteractive-run-policy",
            )
            .await?;
    }
}

/// Cooperative `yield` is a scheduling boundary, not a request for the model
/// or user to intervene.  The interactive wire runner therefore gives other
/// Tokio work a chance to run and then resumes the exact saved execution.
///
/// This intentionally handles *only* [`PendingTypedReason::Yielded`].  A host
/// request, approval, proposal editor, task join, or cancellation remains
/// suspended for its owning application lifecycle; treating any of those as a
/// generic auto-retry would duplicate side effects or bypass a human decision.
/// Durable timer/I/O/message wakeups belong to the later daemon scheduler.
async fn resume_interactive_yields(
    runtime: &crate::runtime::ProgramRuntime,
    mut outcome: crate::runtime::outcome::ExecutionOutcome,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    while outcome.status == crate::runtime::outcome::ExecutionStatus::Suspended
        && matches!(
            runtime.pending_typed_execution(outcome.execution_id)?,
            Some(crate::runtime::PendingTypedExecutionInfo {
                reason: crate::runtime::PendingTypedReason::Yielded,
                yielded_value: Some(crate::programs::ProgramValue::Nil),
                ..
            })
        )
    {
        // Do not spin a whole VM run inside the provider-response task. The
        // typed runtime preserves fuel across continuations, and explicit
        // yielding makes cancellation/presentation events observable between
        // slices.
        tokio::task::yield_now().await;
        outcome = runtime.resume_typed_execution(outcome.execution_id).await?;
    }
    Ok(outcome)
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
    crate::programs::is_repairable_wire_diagnostic(diagnostic)
}

fn wire_repair_messages(
    messages: &[crate::claude::Message],
    rejected_source: &str,
    diagnostic: &str,
) -> Vec<crate::claude::Message> {
    let mut repair_messages = messages.to_vec();
    repair_messages.push(crate::claude::Message::assistant(rejected_source));
    repair_messages.push(crate::claude::Message::user(
        crate::programs::wire_repair_request(rejected_source, diagnostic),
    ));
    repair_messages
}

/// Pick the vocabulary-relevance query for this inference. Tool-result
/// continuations deliberately carry an empty internal `query`, but they are
/// still provider turns and must receive the complete VM wire ABI. Reuse the
/// most recent human text when possible; the fallback still produces the
/// provider-neutral boot manifest when only tool-result blocks remain.
fn vm_manifest_query(messages: &[crate::claude::Message], query: &str) -> String {
    if !query.trim().is_empty() {
        return query.to_string();
    }

    messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .flat_map(|message| message.content.iter())
        .filter_map(ContentBlock::as_text)
        .find(|text| !text.trim().is_empty())
        .unwrap_or("Finch VM wire protocol")
        .to_string()
}

struct WireExecution {
    source_for_history: String,
    response: String,
    effect_journal: Vec<crate::server::RunnerEffectRecord>,
    output_unit: Arc<crate::cli::messages::WorkUnit>,
}

/// What a turn produced, as distinct from the wire program that produced it.
///
/// In its own module so the tuple field is unreachable from this one: the
/// defect being prevented is passing the wrong string, memory having indexed
/// `source_for_history` for long enough to put 9,288 nodes of raw `(say ...)`
/// into the dogfood store from 19 distinct programs (#254). Both are `String`,
/// so nothing but a type distinguishes them, and no test can observe the swap
/// without driving the whole `process_query_with_tools` path.
///
/// A private field in this module would have been a speed bump rather than a
/// guarantee — `RenderedTurn(source_for_history.clone())` would still compile
/// at both call sites.
///
/// Constructing one from anything other than a `WireExecution`'s rendered half
/// is not merely discouraged, it does not compile outside this module: the
/// tuple field is private to `rendered`, and the sole production constructor —
/// `WireExecution::rendered` — lives inside it. Tests get a `#[cfg(test)]`
/// `for_test`, which does not exist in a release build.
mod rendered {
    use super::WireExecution;

    /// The rendered result of a turn.
    pub(super) struct RenderedTurn(String);

    impl RenderedTurn {
        pub(super) fn as_str(&self) -> &str {
            &self.0
        }

        /// Tests need to drive `persist_completed_turn_memory` directly with a
        /// literal. `#[cfg(test)]` so the door does not exist in a release
        /// build.
        #[cfg(test)]
        pub(super) fn for_test(rendered: &str) -> Self {
            Self(rendered.to_string())
        }
    }

    impl WireExecution {
        /// The rendered result, for memory. The only way to obtain a
        /// `RenderedTurn`; a descendant module may read its parent's private
        /// fields, so this needs no constructor taking a bare `&str`.
        pub(super) fn rendered(&self) -> RenderedTurn {
            RenderedTurn(self.response.clone())
        }
    }
}

use rendered::RenderedTurn;

pub(super) fn runner_effect_records(
    outcome: &crate::runtime::outcome::ExecutionOutcome,
) -> Vec<crate::server::RunnerEffectRecord> {
    outcome
        .effect_journal
        .iter()
        .cloned()
        .map(|entry| crate::server::RunnerEffectRecord {
            execution_id: outcome.execution_id,
            entry,
        })
        .collect()
}

fn record_wire_metric(
    logger: Option<&crate::metrics::MetricsLogger>,
    metric: &crate::metrics::WireAdherenceMetric,
) {
    if let Some(logger) = logger {
        if let Err(error) = logger.log_wire(metric) {
            tracing::warn!("failed to record provider wire adherence: {error}");
        }
    }
}

/// Execute one provider wire response and, for a safe rejected program, ask
/// the same model for precisely one source-level correction.  Each source and
/// each output owns a separate WorkUnit, so the failed program never vanishes
/// from scrollback when the replacement succeeds.
async fn execute_wire_with_single_repair(
    runtime: &crate::runtime::ProgramRuntime,
    output_manager: Arc<OutputManager>,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    cancel: tokio_util::sync::CancellationToken,
    generator: Arc<dyn Generator>,
    messages: &[crate::claude::Message],
    source: String,
    metrics_logger: Option<&crate::metrics::MetricsLogger>,
    effect_audit: Option<crate::server::RunnerEffectAuditControl>,
) -> WireExecution {
    let mut metric = crate::metrics::WireAdherenceMetric::first_pass(
        generator.name(),
        generator.model_name(),
        "interactive",
    );
    crate::programs::corpus::capture_with_runtime_from_env(
        runtime,
        generator.name(),
        generator.model_name(),
        "interactive",
        crate::programs::corpus::WireCorpusAttempt::FirstPass,
        &source,
    );
    let output_unit = output_manager.start_work_unit("VM program output");
    output_unit.set_program_output();
    let initial = execute_direct_wire_response(
        runtime,
        Arc::clone(&output_manager),
        Arc::clone(&output_unit),
        event_tx.clone(),
        cancel.clone(),
        source.clone(),
        effect_audit.clone(),
    )
    .await;

    let mut effect_journal = Vec::new();
    let (diagnostic, repairable) = match initial {
        Ok(outcome) if outcome.status == crate::runtime::outcome::ExecutionStatus::Completed => {
            if outcome.output.is_empty() {
                metric.first_pass_valid = false;
                metric.failure_class = Some(crate::metrics::WireFailureClass::MissingOutputEffect);
                metric.terminal_failure = true;
            }
            record_wire_metric(metrics_logger, &metric);
            let _ = event_tx.send(ReplEvent::VmOutputComplete {
                output_unit: Arc::clone(&output_unit),
            });
            let effect_journal = runner_effect_records(&outcome);
            return WireExecution {
                source_for_history: source,
                response: outcome.output,
                effect_journal,
                output_unit,
            };
        }
        Ok(outcome) => {
            effect_journal.extend(runner_effect_records(&outcome));
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

    metric.first_pass_valid = false;
    metric.failure_class = Some(crate::programs::classify_wire_failure(&source, &diagnostic));
    metric.diagnostic_code = crate::programs::wire_diagnostic_code(&diagnostic);

    output_unit.append_response(&format!("VM wire error: {diagnostic}"));
    if !repairable {
        metric.terminal_failure = true;
        record_wire_metric(metrics_logger, &metric);
        let _ = event_tx.send(ReplEvent::VmOutputComplete {
            output_unit: Arc::clone(&output_unit),
        });
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
            effect_journal,
            output_unit,
        };
    }
    if cancel.is_cancelled() {
        metric.terminal_failure = true;
        record_wire_metric(metrics_logger, &metric);
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
            effect_journal,
            output_unit,
        };
    }
    metric.repair_attempted = true;

    // Keep the rejected result visibly live while the bounded corrective
    // inference runs. Previously a plain output WorkUnit showed only the
    // static error, making Finch look dead while subsequent input queued.
    output_unit.set_output_handle("VM program rejected");
    output_unit.set_transient_status(Some(
        "requesting one corrected ProgramSubmission from the provider…".to_string(),
    ));
    let repair_messages = wire_repair_messages(messages, &source, &diagnostic);
    let repair = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            output_unit.set_transient_status(None);
            metric.terminal_failure = true;
            record_wire_metric(metrics_logger, &metric);
            output_unit.set_complete();
            return WireExecution {
                source_for_history: source,
                response: diagnostic,
                effect_journal,
                output_unit,
            };
        }
        repair = generator.generate(repair_messages, None) => repair,
    };
    output_unit.set_transient_status(None);
    if cancel.is_cancelled() {
        metric.terminal_failure = true;
        record_wire_metric(metrics_logger, &metric);
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
            effect_journal,
            output_unit,
        };
    }
    let Ok(repair) = repair else {
        metric.terminal_failure = true;
        record_wire_metric(metrics_logger, &metric);
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
            effect_journal,
            output_unit,
        };
    };
    if !repair.tool_uses.is_empty() || repair.text.trim().is_empty() {
        metric.terminal_failure = true;
        record_wire_metric(metrics_logger, &metric);
        output_unit.set_complete();
        return WireExecution {
            source_for_history: source,
            response: diagnostic,
            effect_journal,
            output_unit,
        };
    }
    output_unit.set_complete();

    let repaired_source = raw_wire_source(&repair.text);
    crate::programs::corpus::capture_with_runtime_from_env(
        runtime,
        generator.name(),
        generator.model_name(),
        "interactive",
        crate::programs::corpus::WireCorpusAttempt::Repair,
        &repaired_source,
    );
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
        event_tx.clone(),
        cancel,
        repaired_source.clone(),
        effect_audit,
    )
    .await
    {
        Ok(outcome) if outcome.status == crate::runtime::outcome::ExecutionStatus::Completed => {
            effect_journal.extend(runner_effect_records(&outcome));
            metric.repaired_successfully = !outcome.output.is_empty();
            metric.terminal_failure = outcome.output.is_empty();
            if outcome.output.is_empty() {
                metric.failure_class = Some(crate::metrics::WireFailureClass::MissingOutputEffect);
            }
            record_wire_metric(metrics_logger, &metric);
            let _ = event_tx.send(ReplEvent::VmOutputComplete {
                output_unit: Arc::clone(&repair_output_unit),
            });
            WireExecution {
                source_for_history: repaired_source,
                response: outcome.output,
                effect_journal,
                output_unit: repair_output_unit,
            }
        }
        Ok(outcome) => {
            effect_journal.extend(runner_effect_records(&outcome));
            let detail = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            repair_output_unit.append_response(&format!("VM wire error: {detail}"));
            let _ = event_tx.send(ReplEvent::VmOutputComplete {
                output_unit: Arc::clone(&repair_output_unit),
            });
            metric.terminal_failure = true;
            record_wire_metric(metrics_logger, &metric);
            WireExecution {
                source_for_history: repaired_source,
                response: detail,
                effect_journal,
                output_unit: repair_output_unit,
            }
        }
        Err(error) => {
            let detail = format!("VM wire error: {error}");
            repair_output_unit.append_response(&detail);
            let _ = event_tx.send(ReplEvent::VmOutputComplete {
                output_unit: Arc::clone(&repair_output_unit),
            });
            metric.terminal_failure = true;
            record_wire_metric(metrics_logger, &metric);
            WireExecution {
                source_for_history: repaired_source,
                response: detail,
                effect_journal,
                output_unit: repair_output_unit,
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

/// Store a completed turn in memory and refresh its Brain-local summary.
///
/// `assistant_rendered` must be what the turn produced, not the wire program
/// that produced it. Indexing emitted source put 9,288 nodes of raw `(say ...)`
/// into the dogfood store from only 19 distinct programs, lexically
/// near-identical to every other program because they all share `(`, `say` and
/// quoting tokens, which is corrosive to a similarity-driven index (#254).
///
/// The source is not hidden from the user — it renders as a `Program source`
/// row, expanded by default at three lines or fewer, which is exactly the
/// `(say ...)` case. It is simply the wrong thing to index.
async fn persist_completed_turn_memory(
    memory_system: &crate::memory::MemorySystem,
    conversation: &Arc<RwLock<ConversationHistory>>,
    query_id: Uuid,
    query_states: &QueryStateManager,
    query: &str,
    assistant_rendered: &RenderedTurn,
    model: &str,
    session_label: &str,
    cwd: &str,
    status_bar: &StatusBar,
    context_lines: usize,
    memory_recall: crate::memory_status::Recall,
) {
    let brain_provenance = query_states
        .get_metadata(query_id)
        .await
        .and_then(|metadata| metadata.brain_turn_provenance);
    // Named-Brain memory is projected only after the daemon has committed the
    // canonical Program/checkpoint/Result sequence. The daemon then calls the
    // exact leased runner back with that durable Brain/run provenance.
    if brain_provenance.is_some() {
        return;
    }
    let explicit_query = query
        .split_once("\n\n[Context:")
        .map(|(raw, _)| raw)
        .unwrap_or(query)
        .trim();
    let user_text = if explicit_query.is_empty() {
        conversation
            .read()
            .await
            .get_messages()
            .into_iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.text())
            .unwrap_or_default()
    } else {
        explicit_query.to_string()
    };
    // Logged rather than discarded.
    //
    // These were `let _ =`, which was survivable while the only failures were
    // transient. It is not survivable now that a failed MemTree hydration
    // refuses writes for the rest of the process (#276): memory capture would
    // stop for the whole session with no signal anywhere, while the status line
    // below went on reporting recalls from the index that had loaded.
    //
    // Not propagated: the turn itself succeeded and the user has their answer,
    // so failing it here would be worse. A warning per turn is the honest
    // middle, until #275 gives memory a status surface of its own.
    if !user_text.is_empty() {
        if let Err(error) = memory_system
            .insert_conversation("user", &user_text, Some(model), Some(session_label))
            .await
        {
            tracing::warn!(%error, "could not store the user turn in memory");
        }
    }
    if !assistant_rendered.as_str().trim().is_empty() {
        if let Err(error) = memory_system
            .insert_conversation(
                "assistant",
                assistant_rendered.as_str(),
                Some(model),
                Some(session_label),
            )
            .await
        {
            tracing::warn!(%error, "could not store the assistant turn in memory");
        }
    }
    status_bar.update_line(
        crate::cli::status_bar::StatusLineType::MemoryContext,
        memory_recall.line_against(memory_system.hydration_status()),
    );
    refresh_context_strip(memory_system, session_label, cwd, status_bar, context_lines).await;
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
    round_token: crate::cli::conversation::ToolRoundToken,
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
    memory_recall: crate::memory_status::Recall,
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

    let _ = event_tx.send(ReplEvent::ToolCallsStarted {
        query_id,
        tool_uses: tool_uses.clone(),
    });
    let effect_audit = query_states
        .get_metadata(query_id)
        .await
        .and_then(|metadata| metadata.effect_audit);
    // The gate below runs against the mode this batch was *authored* under,
    // read once by value so no lock is held across the loop.  Two separate
    // things depend on that:
    //
    // * `handle_present_plan` is awaited inside this loop and, once the user
    //   approves, takes the `mode` **write** lock.  Tokio's `RwLock` is
    //   write-preferring, so a read guard held across that await made the query
    //   task wait on itself: approval resolved and then nothing happened, the
    //   query never terminalized, its lane was never released, and every later
    //   turn queued behind it forever — the wedged session of #363.  The whole
    //   mode surface was stuck too, which is why Escape and an empty prompt did
    //   not recover it either.
    // * The *dispatch gate below* stays fixed for the batch.  Every call here
    //   was authored by the provider while the session was still `Planning`, so
    //   an approval landing part-way through does not make this loop re-gate the
    //   siblings queued behind it: a `write` the model asked for under
    //   `Planning` is still refused here.
    //
    //   Be exact about what that does *not* claim.  A tool that passes this gate
    //   is handed to `spawn_tool_execution`, and the executor re-reads the mode
    //   at execution time (`src/tools/executor.rs:467`), where the block applies
    //   only while the mode is still `Planning`.  A same-batch `bash` therefore
    //   does run after an approval, because by then the mode is `Executing`.
    //   That is the executor's decision rather than this gate's, and it is not
    //   an escalation — the user just approved "all tools enabled" — but this
    //   snapshot fixes the gate, not the batch's effective authority.
    let batch_mode = mode.read().await.clone();
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
                round_token,
                tool_id: tool_use.id.clone(),
                result: Err(anyhow::anyhow!("{}", error_msg)),
            });
            continue;
        }

        // Plan-mode gate: block destructive tools while exploring
        if !is_tool_allowed_in_mode(&tool_use.name, &batch_mode) {
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
                round_token,
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
                round_token,
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
                round_token,
                tool_id: tool_use.id.clone(),
                result,
            });
        } else {
            // Regular tool: run concurrently in a background task
            tool_coordinator.spawn_tool_execution(
                query_id,
                round_token,
                tool_use,
                Arc::clone(work_unit),
                row_idx,
                effect_audit.clone(),
            );
        }
    }

    // Update memory status bar now that tools are queued
    if let Some(ref mem) = memory_system {
        status_bar.update_line(
            crate::cli::status_bar::StatusLineType::MemoryContext,
            memory_recall.line_against(mem.hydration_status()),
        );
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
    streaming_enabled: bool,
    enable_summarization: bool,
    auto_compact_enabled: bool,
    summary_gen: Arc<dyn Generator>,
    tool_call_history: Arc<
        RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>,
    >,
    wire_metrics_logger: Option<Arc<crate::metrics::MetricsLogger>>,
    persona_system_prompt: String,
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
    let mut memory_recall = crate::memory_status::Recall::none();
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
            compactor
                .compact_with_system(&dropped, window, Some(persona_system_prompt.clone()))
                .await
        } else {
            apply_sliding_window(all_msgs, max_verbatim)
        };
        if let Some(ref mem) = memory_system {
            // Sample the index before the query as well as after. Hydration
            // advances while the query runs, so an after-only sample can read
            // `Ready` for a search that covered a fraction of the store --
            // failing open, in the one direction that matters.
            let before = mem.hydration_status();
            let recalled = mem.query(&query, Some(recall_k)).await;
            memory_recall.index = crate::memory_status::observed(before, mem.hydration_status());
            if let Ok(memories) = recalled {
                if !memories.is_empty() {
                    memory_recall.count = memories.len();
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
                }
            }
            // Outside the emptiness guard on purpose. An unusable index recalls
            // nothing, so nesting the update inside `!memories.is_empty()` left
            // the previous turn's line standing at exactly the moment the strip
            // needed to say the memory was unavailable.
            status_bar.update_line(
                crate::cli::status_bar::StatusLineType::MemoryContext,
                memory_recall.line(),
            );
        }
        // This execution contract is required on *every* provider inference,
        // including internal empty-query continuations after tool results.
        // The manifest is local request data rather than persisted conversation
        // history, so it must be re-injected each round trip.
        let manifest_query = vm_manifest_query(&msgs, &query);
        let manifest = match memory_system.as_ref() {
            Some(memory) => memory
                .vm_manifest(&manifest_query, 12)
                .await
                .unwrap_or_else(|_| fallback_vm_manifest()),
            None => fallback_vm_manifest(),
        };
        inject_persona_system_prompt(&mut msgs, persona_system_prompt);
        inject_vm_manifest(&mut msgs, &manifest);
        msgs
    };
    let caps = generator.capabilities();

    // Streaming is both a provider capability and a user preference. The
    // setup wizard persists the latter in features.streaming_enabled.
    if should_stream_responses(streaming_enabled, caps.supports_streaming) {
        tracing::debug!("Generator supports streaming, attempting to stream");

        // Create a WorkUnit for this generation turn BEFORE streaming begins.
        // The shadow-buffer / insert_before architecture requires the message to
        // exist in output_manager before any blit cycles run — the WorkUnit's
        // time-driven animation will be visible during streaming.
        let named_brain_turn = query_states
            .get_metadata(query_id)
            .await
            .is_some_and(|metadata| metadata.brain_turn_provenance.is_some());
        let inherited_tool_unit = if query.is_empty() || named_brain_turn {
            query_states.tool_work_unit(query_id).await
        } else {
            None
        };
        let reusing_tool_unit = inherited_tool_unit.is_some();
        let work_unit = inherited_tool_unit.unwrap_or_else(|| {
            let verb = crate::cli::messages::random_spinner_verb();
            output_manager.start_work_unit(verb)
        });

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

        let stream_cancellation = query_states
            .get_metadata(query_id)
            .await
            .map(|metadata| metadata.cancellation_token)
            .unwrap_or_default();
        match generator
            .generate_stream_cancellable(
                messages.clone(),
                Some((*tool_definitions).clone()),
                stream_cancellation,
            )
            .await
        {
            Ok(Some(mut rx)) => {
                tracing::debug!("[EVENT_LOOP] Streaming started, entering receive loop");
                tracing::debug!("Streaming started successfully");

                // Process stream (handles tools via StreamChunk::ContentBlockComplete)
                let mut blocks = Vec::new();
                let mut text = String::new();
                let mut completed_text = String::new();
                let mut actual_model: Option<String> = None;
                let mut output_token_count: Option<u32> = None;
                let mut primary_allowance_used_percent: Option<f32> = None;
                let mut secondary_allowance_used_percent: Option<f32> = None;

                while let Some(result) = rx.recv().await {
                    match result {
                        Ok(StreamChunk::Usage {
                            input_tokens,
                            output_tokens,
                        }) => {
                            input_token_count = Some(input_tokens);
                            output_token_count = Some(output_tokens);
                        }
                        Ok(StreamChunk::Allowance {
                            primary_used_percent,
                            secondary_used_percent,
                        }) => {
                            primary_allowance_used_percent = primary_used_percent;
                            secondary_allowance_used_percent = secondary_used_percent;
                        }
                        Ok(StreamChunk::ResponseMetadata { model }) => {
                            actual_model = Some(model);
                        }
                        Ok(StreamChunk::TextDelta(delta)) => {
                            tracing::debug!("Received TextDelta: {} bytes", delta.len());
                            text.push_str(&delta);
                            token_count += delta.split_whitespace().count();
                            // A continuation inference belongs to the existing
                            // Tools activity until we know whether it produced
                            // more calls or a final wire program. Buffer its
                            // text rather than replacing the tool block.
                            work_unit.add_tokens(&delta);
                            // Every text-only response is candidate VM source.
                            // Project its exact bytes immediately; parsing and
                            // verification still wait for the complete source.
                            if !reusing_tool_unit && has_streamed_wire_source(&text) {
                                let language =
                                    crate::programs::ProgramLanguage::infer_source(&text);
                                work_unit.set_program_source(language.as_str());
                                work_unit.set_response(&text);
                            }
                        }
                        Ok(StreamChunk::ContentBlockComplete(block)) => {
                            tracing::debug!(
                                block_type = match &block {
                                    ContentBlock::Text { .. } => "text",
                                    ContentBlock::Image { .. } => "image",
                                    ContentBlock::ToolUse { .. } => "tool_use",
                                    ContentBlock::ToolResult { .. } => "tool_result",
                                    ContentBlock::OpaqueReasoning { .. } => "opaque_reasoning",
                                },
                                "Received completed provider content block"
                            );
                            if let ContentBlock::Text { text } = &block {
                                completed_text.push_str(text);
                            }
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

                if text.is_empty() {
                    text.clone_from(&completed_text);
                    if !text.is_empty() {
                        token_count = text.split_whitespace().count();
                        work_unit.add_tokens(&text);
                    }
                } else if !completed_text.is_empty() && completed_text != text {
                    work_unit.set_failed();
                    let _ = event_tx.send(ReplEvent::QueryFailed {
                        query_id,
                        error: "Provider streaming text did not match its completed content"
                            .to_string(),
                    });
                    return;
                }
                let actual_model =
                    actual_model.unwrap_or_else(|| generator.model_name().to_string());

                query_states
                    .set_invocation_metadata(
                        query_id,
                        crate::providers::types::InvocationMetadata {
                            requested_model: generator.model_name().to_string(),
                            resolved_model: generator.model_name().to_string(),
                            actual_model: actual_model.clone(),
                            input_tokens: input_token_count,
                            output_tokens: output_token_count.or(Some(token_count as u32)),
                            primary_allowance_used_percent,
                            secondary_allowance_used_percent,
                        },
                    )
                    .await;

                // Send stats update
                let _ = event_tx.send(ReplEvent::StatsUpdate {
                    model: actual_model.clone(),
                    input_tokens: input_token_count,
                    output_tokens: output_token_count.or(Some(token_count as u32)),
                    latency_ms: Some(stream_start.elapsed().as_millis() as u64),
                    primary_allowance_used_percent,
                    secondary_allowance_used_percent,
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
                    // Text accompanying a tool-use response is provider
                    // scratch narration, not an executable Finch wire
                    // program. Keep only the structured calls in the stable
                    // query-level tool activity block.
                    work_unit.set_assistant_presentation();
                    work_unit.set_response("");
                    query_states
                        .set_tool_work_unit(query_id, Some(Arc::clone(&work_unit)))
                        .await;
                    tracing::debug!("[EVENT_LOOP] Query state updated, staging assistant message");
                    // Keep the tool-bearing assistant message invisible until
                    // all matching results can be committed atomically.
                    let assistant_message = crate::claude::Message {
                        role: "assistant".to_string(),
                        content: blocks.clone(),
                    };
                    tracing::debug!("[EVENT_LOOP] Acquiring conversation write lock...");
                    let round_token = match conversation
                        .write()
                        .await
                        .stage_assistant(query_id, assistant_message)
                    {
                        Ok(token) => token,
                        Err(crate::cli::conversation::ToolRoundError::StageAlreadyExists) => {
                            tracing::warn!(
                                "Ignoring duplicate provider tool completion for query {}",
                                query_id
                            );
                            work_unit.set_complete();
                            return;
                        }
                        Err(error) => {
                            work_unit.set_failed();
                            let _ = event_tx.send(ReplEvent::QueryFailed {
                                query_id,
                                error: format!("Could not stage tool round: {error}"),
                            });
                            return;
                        }
                    };
                    if !query_states
                        .begin_tool_execution(query_id, tool_uses.len())
                        .await
                    {
                        conversation.write().await.abort_staged(query_id);
                        return;
                    }
                    tracing::debug!(
                        "[EVENT_LOOP] Assistant message added, spawning tool executions"
                    );

                    // Dispatch tools (loop detection, mode gating, inline handlers, spawn)
                    dispatch_tool_uses(
                        tool_uses,
                        query_id,
                        round_token,
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
                        memory_recall.clone(),
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
                let source_unit = if reusing_tool_unit && !named_brain_turn {
                    work_unit.set_complete();
                    query_states.set_tool_work_unit(query_id, None).await;
                    output_manager.start_work_unit(crate::cli::messages::random_spinner_verb())
                } else {
                    Arc::clone(&work_unit)
                };
                let wire_source = raw_wire_source(&text);
                let wire_language = crate::programs::ProgramLanguage::infer_source(&wire_source);
                source_unit.set_program_source(wire_language.as_str());
                source_unit.set_response(wire_source.clone());
                source_unit.set_complete();
                let query_metadata = query_states.get_metadata(query_id).await;
                let cancel = query_metadata
                    .as_ref()
                    .map(|metadata| metadata.cancellation_token.clone())
                    .unwrap_or_default();
                let effect_audit = query_metadata.and_then(|metadata| metadata.effect_audit);
                let wire_execution = execute_wire_with_single_repair(
                    program_runtime.as_ref(),
                    Arc::clone(&output_manager),
                    event_tx.clone(),
                    cancel,
                    Arc::clone(&generator),
                    &messages,
                    wire_source.clone(),
                    wire_metrics_logger.as_deref(),
                    effect_audit,
                )
                .await;
                if query_states
                    .get_metadata(query_id)
                    .await
                    .is_some_and(|metadata| metadata.brain_turn_provenance.is_some())
                {
                    query_states
                        .set_brain_output_work_unit(
                            query_id,
                            Some(Arc::clone(&wire_execution.output_unit)),
                        )
                        .await;
                }
                let wire_execution_rendered = wire_execution.rendered();
                let response = wire_execution.response;
                let source_for_history = wire_execution.source_for_history;
                let history_content = match history_content_with_source(&blocks, source_for_history)
                {
                    Ok(content) => content,
                    Err(error) => {
                        work_unit.set_failed();
                        let _ = event_tx.send(ReplEvent::QueryFailed {
                            query_id,
                            error: error.to_string(),
                        });
                        return;
                    }
                };
                let effect_journal = wire_execution.effect_journal;
                let published = query_states
                    .try_publish_completion_content(
                        query_id,
                        response.clone(),
                        history_content,
                        &conversation,
                    )
                    .await;
                let _ = event_tx.send(ReplEvent::VmEffectJournalComplete {
                    query_id,
                    records: effect_journal,
                });
                let _ = event_tx.send(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: response,
                });
                if published {
                    if let Some(ref mem) = memory_system {
                        // `wire_execution_rendered`, not `source_for_history`:
                        // memory indexes what the turn produced, not the
                        // program that produced it. 55% of the dogfood store
                        // was raw `(say ...)` source, which shares `(`, `say`
                        // and quoting tokens with every other program, making
                        // all programs near-identical under a lexical
                        // embedding (#254). The source is not hidden from the
                        // user — it renders as a `Program source` row — it is
                        // simply the wrong thing to index.
                        persist_completed_turn_memory(
                            mem,
                            &conversation,
                            query_id,
                            query_states.as_ref(),
                            &query,
                            &wire_execution_rendered,
                            &actual_model,
                            &session_label,
                            &cwd,
                            &status_bar,
                            context_lines,
                            memory_recall.clone(),
                        )
                        .await;
                    }
                }
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
    let named_brain_turn = query_states
        .get_metadata(query_id)
        .await
        .is_some_and(|metadata| metadata.brain_turn_provenance.is_some());
    let inherited_tool_unit = if query.is_empty() || named_brain_turn {
        query_states.tool_work_unit(query_id).await
    } else {
        None
    };
    let reusing_tool_unit = inherited_tool_unit.is_some();
    let work_unit = inherited_tool_unit.unwrap_or_else(|| {
        let verb = crate::cli::messages::random_spinner_verb();
        output_manager.start_work_unit(verb)
    });
    match generator
        .generate(messages.clone(), Some((*tool_definitions).clone()))
        .await
    {
        Ok(response) => {
            query_states
                .set_invocation_metadata(
                    query_id,
                    crate::providers::types::InvocationMetadata {
                        requested_model: generator.model_name().to_string(),
                        resolved_model: generator.model_name().to_string(),
                        actual_model: response.metadata.model.clone(),
                        input_tokens: response.metadata.input_tokens,
                        output_tokens: response.metadata.output_tokens,
                        primary_allowance_used_percent: response
                            .metadata
                            .primary_allowance_used_percent,
                        secondary_allowance_used_percent: response
                            .metadata
                            .secondary_allowance_used_percent,
                    },
                )
                .await;
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
                primary_allowance_used_percent: response.metadata.primary_allowance_used_percent,
                secondary_allowance_used_percent: response
                    .metadata
                    .secondary_allowance_used_percent,
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
                work_unit.set_assistant_presentation();
                work_unit.set_response("");
                query_states
                    .set_tool_work_unit(query_id, Some(Arc::clone(&work_unit)))
                    .await;
                // Keep the tool-bearing assistant message invisible until
                // all matching results can be committed atomically.
                let assistant_message = crate::claude::Message {
                    role: "assistant".to_string(),
                    content: response.content_blocks.clone(),
                };
                let round_token = match conversation
                    .write()
                    .await
                    .stage_assistant(query_id, assistant_message)
                {
                    Ok(token) => token,
                    Err(crate::cli::conversation::ToolRoundError::StageAlreadyExists) => {
                        tracing::warn!(
                            "Ignoring duplicate provider tool completion for query {}",
                            query_id
                        );
                        work_unit.set_complete();
                        return;
                    }
                    Err(error) => {
                        work_unit.set_failed();
                        let _ = event_tx.send(ReplEvent::QueryFailed {
                            query_id,
                            error: format!("Could not stage tool round: {error}"),
                        });
                        return;
                    }
                };
                if !query_states
                    .begin_tool_execution(query_id, tool_uses.len())
                    .await
                {
                    conversation.write().await.abort_staged(query_id);
                    return;
                }

                // Dispatch tools (loop detection, mode gating, inline handlers, spawn)
                dispatch_tool_uses(
                    tool_uses,
                    query_id,
                    round_token,
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
                    memory_recall.clone(),
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
            let source_unit = if reusing_tool_unit && !named_brain_turn {
                work_unit.set_complete();
                query_states.set_tool_work_unit(query_id, None).await;
                output_manager.start_work_unit(crate::cli::messages::random_spinner_verb())
            } else {
                Arc::clone(&work_unit)
            };
            let wire_source = raw_wire_source(&response.text);
            let wire_language = crate::programs::ProgramLanguage::infer_source(&wire_source);
            source_unit.set_program_source(wire_language.as_str());
            source_unit.set_response(wire_source.clone());
            source_unit.set_complete();
            let query_metadata = query_states.get_metadata(query_id).await;
            let cancel = query_metadata
                .as_ref()
                .map(|metadata| metadata.cancellation_token.clone())
                .unwrap_or_default();
            let effect_audit = query_metadata.and_then(|metadata| metadata.effect_audit);
            let wire_execution = execute_wire_with_single_repair(
                program_runtime.as_ref(),
                Arc::clone(&output_manager),
                event_tx.clone(),
                cancel,
                Arc::clone(&generator),
                &messages,
                wire_source.clone(),
                wire_metrics_logger.as_deref(),
                effect_audit,
            )
            .await;
            if query_states
                .get_metadata(query_id)
                .await
                .is_some_and(|metadata| metadata.brain_turn_provenance.is_some())
            {
                query_states
                    .set_brain_output_work_unit(
                        query_id,
                        Some(Arc::clone(&wire_execution.output_unit)),
                    )
                    .await;
            }
            let wire_execution_rendered = wire_execution.rendered();
            let rendered_response = wire_execution.response;
            let effect_journal = wire_execution.effect_journal;
            let source_for_history = wire_execution.source_for_history;
            let history_content =
                match history_content_with_source(&response.content_blocks, source_for_history) {
                    Ok(content) => content,
                    Err(error) => {
                        work_unit.set_failed();
                        let _ = event_tx.send(ReplEvent::QueryFailed {
                            query_id,
                            error: error.to_string(),
                        });
                        return;
                    }
                };
            let published = query_states
                .try_publish_completion_content(
                    query_id,
                    rendered_response.clone(),
                    history_content,
                    &conversation,
                )
                .await;
            let _ = event_tx.send(ReplEvent::VmEffectJournalComplete {
                query_id,
                records: effect_journal,
            });
            let _ = event_tx.send(ReplEvent::StreamingComplete {
                query_id,
                full_response: rendered_response,
            });
            tracing::debug!("Query complete (no tools), non-streaming finished");

            // Store the completed turn and refresh its Brain-local summary.
            if published {
                if let Some(ref mem) = memory_system {
                    let model_name = response.metadata.model.clone();
                    // As above: the rendered result, not the source.
                    persist_completed_turn_memory(
                        mem,
                        &conversation,
                        query_id,
                        query_states.as_ref(),
                        &query,
                        &wire_execution_rendered,
                        &model_name,
                        &session_label,
                        &cwd,
                        &status_bar,
                        context_lines,
                        memory_recall.clone(),
                    )
                    .await;
                }
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

fn should_stream_responses(streaming_enabled: bool, provider_supports_streaming: bool) -> bool {
    streaming_enabled && provider_supports_streaming
}

fn inject_persona_system_prompt(
    messages: &mut Vec<crate::claude::Message>,
    persona_system_prompt: String,
) {
    messages.insert(
        0,
        crate::claude::Message {
            role: "system".to_string(),
            content: vec![ContentBlock::Text {
                text: persona_system_prompt,
            }],
        },
    );
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
/// (the start of the next still-valid round-trip), the most recent dropped human
/// request is restored as its user anchor instead of cascading removal. Without
/// this, removing the orphan and following assistant would orphan the *next*
/// tool_result; using a generic placeholder instead loses the task semantics.
pub(crate) fn apply_sliding_window(
    msgs: Vec<crate::claude::Message>,
    max: usize,
) -> Vec<crate::claude::Message> {
    let keep_from = if max == 0 || msgs.len() <= max {
        0
    } else {
        msgs.len() - max
    };
    // Tool-result continuations may consume the entire verbatim window. Keep
    // the latest dropped human request available as their semantic anchor;
    // replacing it with a generic "context omitted" user turn made providers
    // reasonably conclude that no actual task had been supplied.
    let boundary_request = msgs[..keep_from]
        .iter()
        .rev()
        .find(|message| {
            message.role == "user"
                && message.content.iter().any(
                    |block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()),
                )
                && !message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
        .cloned();
    let mut window = msgs[keep_from..].to_vec();
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
                boundary_request.clone().unwrap_or_else(|| crate::claude::Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "[Internal context boundary: this is not a new user request. Continue the retained tool-call transcript and complete its existing request.]".to_string(),
                    }],
                }),
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

    // ── #363: approving a plan must not wedge the session ─────────────────────
    //
    // `dispatch_tool_uses` used to bind a `ReplMode` read guard before its tool
    // loop and hold it across the awaited `handle_present_plan`.  Approval takes
    // the `mode` **write** lock, and tokio's `RwLock` is write-preferring, so the
    // query task ended up waiting on a guard it was holding itself.  What the
    // maintainer saw: "plan mode still does nothing on approval of plan", and
    // then "it permanently breaks entry of new turn prompts ... The brain is
    // dead."  Both halves are the same deadlock — the turn never terminalizes,
    // so its lane is never released, and the whole `ReplMode` surface stays
    // locked, which is why Escape and an empty prompt did not recover it.
    //
    // These regressions drive the real `dispatch_tool_uses` → `ShowDialog` →
    // dialog decision → `ToolResult` path with a real `ToolExecutionCoordinator`
    // and a real headless `TuiRenderer`.  The next-turn half is pinned at the
    // layer the wedge actually occupied: shared `ReplMode` ownership plus a
    // dispatch that returns.
    //
    // They stop short of `EventLoop::execute_query_inner`, the admission gate
    // that parks a later turn in `pending_queries` behind `active_query_id`.
    // That is a choice, not a limitation — `EventLoop::new_named_brain_test_runner`
    // (`event_loop.rs:2103`, `#[cfg(test)]`) does construct an `EventLoop`, and
    // `src/ipc/server.rs:3959` uses it — so the gate is reachable from a test.
    // It is left alone here because that constructor is the named-Brain runner
    // path rather than the interactive one, and because `pending_queries` has a
    // separate pre-existing defect of its own: `CancelQuery` clears
    // `active_query_id` without draining the queue (`event_loop.rs:4699`, versus
    // the drains at `:4327` and `:4598`), so a queued turn re-fires out of order
    // or is lost.  Read `pending_queries` admission and drain as
    // known-uncovered here, not as covered.

    const PLAN_DISPATCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

    type ObservedToolResult = (Uuid, String, std::result::Result<String, String>);

    struct PlanDispatchFixture {
        _temp: tempfile::TempDir,
        plan_path: std::path::PathBuf,
        mode: Arc<RwLock<ReplMode>>,
        conversation: Arc<RwLock<ConversationHistory>>,
        query_states: Arc<QueryStateManager>,
        tui: Arc<tokio::sync::Mutex<TuiRenderer>>,
        output: Arc<OutputManager>,
        status: Arc<StatusBar>,
        coordinator: Arc<ToolExecutionCoordinator>,
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        event_rx: mpsc::UnboundedReceiver<ReplEvent>,
        active_tool_uses: ActiveToolUsesMap,
        tool_call_history:
            Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,
        events: Vec<String>,
        results: Vec<ObservedToolResult>,
    }

    impl PlanDispatchFixture {
        async fn planning() -> Self {
            let temp = tempfile::tempdir().expect("plan dispatch fixture needs a temp directory");
            let plan_path = temp.path().join("plan.md");
            let colors = crate::config::ColorScheme::default();
            // `TuiRenderer::new_headless` already calls `disable_stdout()` on
            // this manager; do not repeat it here.
            let output = Arc::new(OutputManager::new(colors.clone()));
            let status = Arc::new(StatusBar::new());
            let tui = Arc::new(tokio::sync::Mutex::new(TuiRenderer::new_headless(
                Arc::clone(&output),
                Arc::clone(&status),
                colors,
            )));
            let mode = Arc::new(RwLock::new(ReplMode::Planning {
                task: "repair plan approval".into(),
                plan_path: plan_path.clone(),
                created_at: chrono::Utc::now(),
            }));
            let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let executor = Arc::new(tokio::sync::Mutex::new(
                crate::tools::executor::ToolExecutor::new(
                    crate::tools::registry::ToolRegistry::new(),
                    crate::tools::permissions::PermissionManager::new(),
                    temp.path().join("tool-patterns.json"),
                )
                .expect("fixture tool executor must initialize"),
            ));
            let coordinator = Arc::new(ToolExecutionCoordinator::new(
                event_tx.clone(),
                executor,
                Arc::clone(&output),
                Arc::clone(&conversation),
                Arc::new(RwLock::new(crate::local::LocalGenerator::new())),
                Arc::new(
                    crate::models::TextTokenizer::stub().expect("fixture needs a stub tokenizer"),
                ),
                Arc::clone(&mode),
                Arc::new(RwLock::new(None)),
            ));
            Self {
                _temp: temp,
                plan_path,
                mode,
                conversation,
                query_states: Arc::new(QueryStateManager::new()),
                tui,
                output,
                status,
                coordinator,
                event_tx,
                event_rx,
                active_tool_uses: Arc::new(RwLock::new(std::collections::HashMap::new())),
                tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
                events: Vec::new(),
                results: Vec::new(),
            }
        }

        /// Stage one provider turn exactly as the streaming path does, so the
        /// dispatch under test sees a real `ExecutingTools` query and a real
        /// tool-round token.
        async fn begin_turn(
            &self,
            tool_uses: &[ToolUse],
        ) -> (Uuid, crate::cli::conversation::ToolRoundToken) {
            let query_id = self.query_states.create_query(Vec::new()).await;
            let assistant = crate::claude::Message {
                role: "assistant".into(),
                content: tool_uses
                    .iter()
                    .map(|tool_use| ContentBlock::ToolUse {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        input: tool_use.input.clone(),
                    })
                    .collect(),
            };
            let round_token = self
                .conversation
                .write()
                .await
                .stage_assistant(query_id, assistant)
                .expect("fixture must stage the provider's tool call");
            assert!(
                self.query_states
                    .begin_tool_execution(query_id, tool_uses.len())
                    .await,
                "a staged turn must enter ExecutingTools before dispatch; query_state={:?}",
                self.query_states.get_state(query_id).await
            );
            let state = self.query_states.get_state(query_id).await;
            assert!(
                matches!(state, Some(QueryState::ExecutingTools { .. })),
                "these regressions only mean something against a real in-flight tool round; \
                 query_state={state:?}"
            );
            (query_id, round_token)
        }

        async fn cancel_token(&self, query_id: Uuid) -> tokio_util::sync::CancellationToken {
            self.query_states
                .get_metadata(query_id)
                .await
                .expect("a live query must expose its cancellation token")
                .cancellation_token
        }

        fn spawn_dispatch(
            &self,
            tool_uses: Vec<ToolUse>,
            query_id: Uuid,
            round_token: crate::cli::conversation::ToolRoundToken,
        ) -> tokio::task::JoinHandle<()> {
            let work_unit = self.output.start_work_unit("planning");
            let mode = Arc::clone(&self.mode);
            let tool_call_history = Arc::clone(&self.tool_call_history);
            let event_tx = self.event_tx.clone();
            let active_tool_uses = Arc::clone(&self.active_tool_uses);
            let tui = Arc::clone(&self.tui);
            let output = Arc::clone(&self.output);
            let query_states = Arc::clone(&self.query_states);
            let coordinator = Arc::clone(&self.coordinator);
            let status = Arc::clone(&self.status);
            tokio::spawn(async move {
                dispatch_tool_uses(
                    tool_uses,
                    query_id,
                    round_token,
                    &work_unit,
                    &mode,
                    &tool_call_history,
                    &event_tx,
                    &active_tool_uses,
                    &tui,
                    &output,
                    &query_states,
                    coordinator.as_ref(),
                    &None,
                    crate::memory_status::Recall::none(),
                    "plan-approval-regression",
                    "/workspace",
                    &status,
                    0,
                )
                .await;
            })
        }

        /// Consume events until the plan dialog is published, recording every
        /// event and tool result on the way for failure diagnostics.
        async fn await_plan_dialog(
            &mut self,
            what: &str,
        ) -> tokio::sync::oneshot::Sender<crate::cli::tui::DialogResult> {
            let event_rx = &mut self.event_rx;
            let events = &mut self.events;
            let results = &mut self.results;
            let found = tokio::time::timeout(PLAN_DISPATCH_DEADLINE, async {
                loop {
                    let event = event_rx.recv().await?;
                    events.push(format!("{event:?}"));
                    match event {
                        ReplEvent::ShowDialog { response_tx, .. } => return Some(response_tx),
                        ReplEvent::ToolResult {
                            query_id,
                            tool_id,
                            result,
                            ..
                        } => results.push((query_id, tool_id, result.map_err(|e| e.to_string()))),
                        _ => {}
                    }
                }
            })
            .await;
            match found {
                Ok(Some(response_tx)) => response_tx,
                Ok(None) => panic!(
                    "{what}: the frontend event channel closed before dispatch published the plan \
                     approval dialog; events={:?}, results={:?}",
                    self.events, self.results
                ),
                Err(_) => panic!(
                    "{what}: dispatch published no plan approval dialog within \
                     {PLAN_DISPATCH_DEADLINE:?}; events={:?}, results={:?}",
                    self.events, self.results
                ),
            }
        }

        /// Wait for one dispatch to return.  A dispatch that does not return is
        /// exactly the #363 wedge, so the timeout reports the state a maintainer
        /// needs rather than hanging the suite.
        async fn join_dispatch(
            &mut self,
            mut handle: tokio::task::JoinHandle<()>,
            query_id: Uuid,
            what: &str,
        ) {
            match tokio::time::timeout(PLAN_DISPATCH_DEADLINE, &mut handle).await {
                Ok(joined) => {
                    joined.unwrap_or_else(|error| {
                        panic!("{what}: the dispatch task panicked: {error}")
                    });
                }
                Err(_) => {
                    handle.abort();
                    let _ = tokio::time::timeout(PLAN_DISPATCH_DEADLINE, &mut handle).await;
                    let observed_mode = match tokio::time::timeout(
                        PLAN_DISPATCH_DEADLINE,
                        self.mode.read(),
                    )
                    .await
                    {
                        Ok(guard) => format!("{:?}", *guard),
                        Err(_) => "<ReplMode lock still owned after aborting dispatch>".into(),
                    };
                    let state = self.query_states.get_state(query_id).await;
                    panic!(
                        "{what}: dispatch_tool_uses did not return within \
                         {PLAN_DISPATCH_DEADLINE:?}. The interactive query task is wedged, so this \
                         turn never terminalizes, its lane is never released, and every later turn \
                         is stranded (#363). mode={observed_mode}, query_state={state:?}, \
                         events={:?}, results={:?}",
                        self.events, self.results
                    );
                }
            }
        }

        fn drain_events(&mut self) {
            while let Ok(event) = self.event_rx.try_recv() {
                self.events.push(format!("{event:?}"));
                if let ReplEvent::ToolResult {
                    query_id,
                    tool_id,
                    result,
                    ..
                } = event
                {
                    self.results
                        .push((query_id, tool_id, result.map_err(|e| e.to_string())));
                }
            }
        }

        fn results_for(&self, query_id: Uuid) -> Vec<&ObservedToolResult> {
            self.results
                .iter()
                .filter(|(id, _, _)| *id == query_id)
                .collect()
        }

        fn rendered_output(&self) -> String {
            let colors = crate::config::ColorScheme::default();
            self.output
                .get_messages()
                .iter()
                .map(|message| message.format(&colors))
                .collect::<Vec<_>>()
                .join("\n")
        }

        async fn observed_mode(&self) -> ReplMode {
            self.mode.read().await.clone()
        }
    }

    fn present_plan_call(id: &str, plan: &str) -> ToolUse {
        ToolUse {
            id: id.into(),
            name: "present_plan".into(),
            input: serde_json::json!({ "plan": plan }),
        }
    }

    /// The maintainer's first symptom: "plan mode still does nothing on approval
    /// of plan."  Approval must reach `Executing` and the dispatch must return.
    #[tokio::test]
    async fn test_plan_approval_reaches_executing_without_wedging_the_query() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "Implement the approved change.");
        let (query_id, round_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let dispatch = fixture.spawn_dispatch(vec![call], query_id, round_token);

        let response_tx = fixture.await_plan_dialog("plan approval").await;
        response_tx
            .send(crate::cli::tui::DialogResult::Selected(0))
            .expect("the plan dialog receiver disappeared before the user could approve");
        fixture
            .join_dispatch(dispatch, query_id, "plan approval")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Executing { .. }),
            "approving a plan must transition Planning -> Executing so implementation can start; \
             mode={mode:?}, query_state={:?}, events={:?}, results={:?}",
            fixture.query_states.get_state(query_id).await,
            fixture.events,
            fixture.results
        );
        let results = fixture.results_for(query_id);
        assert_eq!(
            results.len(),
            1,
            "one present_plan call must terminalize exactly once; mode={mode:?}, events={:?}, \
             results={:?}",
            fixture.events,
            fixture.results
        );
        let (_, tool_id, result) = results[0];
        assert!(
            tool_id == "present-plan-1" && result.is_ok(),
            "the sole ToolResult must carry the original tool id and succeed, or the provider \
             cannot continue; mode={mode:?}, results={:?}, events={:?}",
            fixture.results,
            fixture.events
        );
        assert!(
            result
                .as_ref()
                .is_ok_and(|text| text.contains("Plan approved by user")
                    && text.contains("Implement the approved change.")),
            "the approved result must hand the plan back to the provider to execute; \
             results={:?}, events={:?}",
            fixture.results,
            fixture.events
        );
        assert_eq!(
            std::fs::read_to_string(&fixture.plan_path).ok().as_deref(),
            Some("Implement the approved change."),
            "the reviewed plan must be the one persisted; results={:?}",
            fixture.results
        );
    }

    /// The maintainer's second symptom, and the one that makes this a demo
    /// blocker: "it permanently breaks entry of new turn prompts. escape, empty
    /// inputs + enter also don't reset it. The brain is dead."
    ///
    /// Both halves of that are shared `ReplMode` ownership.  After an approval
    /// the mode surface must be writable again — that is the `/plan`, Escape and
    /// mode-reset path — and a following turn's dispatch must run to completion.
    #[tokio::test]
    async fn test_a_turn_after_plan_approval_is_still_accepted() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "Ship the reviewed change.");
        let (first_id, round_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let dispatch = fixture.spawn_dispatch(vec![call], first_id, round_token);
        let response_tx = fixture.await_plan_dialog("plan approval").await;
        response_tx
            .send(crate::cli::tui::DialogResult::Selected(0))
            .expect("the plan dialog receiver disappeared before the user could approve");
        fixture
            .join_dispatch(dispatch, first_id, "plan approval")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Executing { .. }),
            "the next-turn regression is only meaningful once approval itself worked; mode={mode:?}, \
             events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );

        // Escape, `/plan` and every mode reset need the write lock.  The wedge
        // held it forever, which is why no keystroke recovered the session.
        let reset_probe = tokio::spawn({
            let mode = Arc::clone(&fixture.mode);
            async move { mode.write().await.clone() }
        });
        let observed = tokio::time::timeout(PLAN_DISPATCH_DEADLINE, reset_probe)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "no task could take the ReplMode write lock within {PLAN_DISPATCH_DEADLINE:?} \
                     after plan approval, so Escape and /plan cannot reset the session (#363); \
                     events={:?}, results={:?}",
                    fixture.events, fixture.results
                )
            })
            .expect("the mode-reset probe task panicked");
        assert!(
            matches!(observed, ReplMode::Executing { .. }),
            "the mode-reset path must observe the approved mode, not a stale one; observed={observed:?}"
        );

        // A following turn dispatches through the same shared mode.  On the
        // wedged build this never got past acquiring it.
        let next_call = present_plan_call("present-plan-2", "A later turn's tool call.");
        let (second_id, second_token) = fixture.begin_turn(std::slice::from_ref(&next_call)).await;
        let second_dispatch = fixture.spawn_dispatch(vec![next_call], second_id, second_token);
        fixture
            .join_dispatch(second_dispatch, second_id, "the turn after approval")
            .await;
        fixture.drain_events();

        assert_ne!(
            first_id, second_id,
            "the two turns must be distinct queries for this assertion to mean anything"
        );
        assert_eq!(
            fixture.results_for(first_id).len(),
            1,
            "the approved turn must stay terminal exactly once after a later turn runs; \
             events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        let second = fixture.results_for(second_id);
        assert_eq!(
            second.len(),
            1,
            "the turn after plan approval must be accepted and terminalize exactly once — this is \
             the 'brain is dead' half of #363; events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        assert!(
            second[0].2.is_ok(),
            "the later turn must produce a usable result; results={:?}, events={:?}",
            fixture.results,
            fixture.events
        );
    }

    /// Rejection takes the same write lock as approval and deadlocked the same
    /// way, so it needs its own regression rather than an argument by analogy.
    #[tokio::test]
    async fn test_plan_rejection_returns_to_normal_mode_without_wedging_the_query() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "A plan the user does not want.");
        let (query_id, round_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let dispatch = fixture.spawn_dispatch(vec![call], query_id, round_token);

        let response_tx = fixture.await_plan_dialog("plan rejection").await;
        response_tx
            .send(crate::cli::tui::DialogResult::Selected(2))
            .expect("the plan dialog receiver disappeared before the user could reject");
        fixture
            .join_dispatch(dispatch, query_id, "plan rejection")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Normal),
            "rejecting a plan must return the session to Normal; mode={mode:?}, events={:?}, \
             results={:?}",
            fixture.events,
            fixture.results
        );
        let results = fixture.results_for(query_id);
        assert_eq!(
            results.len(),
            1,
            "a rejected present_plan must terminalize exactly once; mode={mode:?}, events={:?}, \
             results={:?}",
            fixture.events,
            fixture.results
        );
        assert!(
            results[0]
                .2
                .as_ref()
                .is_ok_and(|text| text.contains("Plan rejected by user")),
            "the provider must be told the plan was rejected; results={:?}, events={:?}",
            fixture.results,
            fixture.events
        );
    }

    /// Ctrl-C while the approval dialog is still unanswered.  The session must
    /// stay exactly where it was — `Planning`, no approval banner — and the
    /// turn must still terminalize exactly once so its lane is released.
    ///
    /// Deterministic on every build, including the base revision, which is
    /// already correct here: with nothing in the dialog channel there is only
    /// one ready `select!` arm to take.  This is a behaviour lock, not a
    /// fail-before case.  What fails on base is the deadlock set above.
    #[tokio::test(flavor = "current_thread")]
    async fn test_cancelling_an_unanswered_plan_approval_keeps_the_session_planning() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "A plan abandoned mid-review.");
        let (query_id, round_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let cancel = fixture.cancel_token(query_id).await;
        let dispatch = fixture.spawn_dispatch(vec![call], query_id, round_token);

        let response_tx = fixture.await_plan_dialog("unanswered plan approval").await;
        cancel.cancel();

        fixture
            .join_dispatch(dispatch, query_id, "unanswered plan approval")
            .await;
        fixture.drain_events();
        drop(response_tx);

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Planning { .. }),
            "cancelling an unanswered plan review must leave the session in Planning; \
             mode={mode:?}, events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        let rendered = fixture.rendered_output();
        assert!(
            !rendered.contains("Plan approved"),
            "an unanswered, cancelled review must not report an approval; rendered={rendered:?}, \
             mode={mode:?}, results={:?}",
            fixture.results
        );
        let results = fixture.results_for(query_id);
        assert_eq!(
            results.len(),
            1,
            "a cancelled present_plan must still terminalize exactly once, or its lane is never \
             released; mode={mode:?}, events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        assert!(
            results[0]
                .2
                .as_ref()
                .is_ok_and(|text| text.contains("cancelled")),
            "the sole result must report the cancellation; results={:?}, rendered={rendered:?}",
            fixture.results
        );
    }

    /// The hostile schedule: the user's approval and a Ctrl-C become ready in
    /// the same poll.  Cancellation must win, so a turn the user abandoned is
    /// never followed by an `Executing` transition or a visible "Plan
    /// approved!".
    ///
    /// `flavor = "current_thread"` is load-bearing rather than incidental.  The
    /// case only exists because the dispatch task is not polled between the send
    /// and the cancel; on a multi-thread runtime it could observe the approval
    /// first, and this would then fail on a *correct* build.
    ///
    /// Fail-before provenance, stated plainly: this case is pinned by mutation,
    /// not by the base revision.  Biasing the `select!` back toward the dialog
    /// arm and deleting the post-select discard fails it deterministically, with
    /// the approved `ToolResult` and the `Executing` mode in the message.  The
    /// base revision is *not* cited as the negative, because base's `select!` is
    /// unbiased: it takes the cancellation arm about half the time and is
    /// accidentally right when it does (measured 18 failures in 40 isolated
    /// runs).  A coin flip is not regression evidence.
    #[tokio::test(flavor = "current_thread")]
    async fn test_a_simultaneous_cancel_and_plan_approval_resolves_as_cancelled() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "A plan the user cancels out of.");
        let (query_id, round_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let cancel = fixture.cancel_token(query_id).await;
        let dispatch = fixture.spawn_dispatch(vec![call], query_id, round_token);

        let response_tx = fixture.await_plan_dialog("cancelled plan approval").await;
        // Nothing awaits between these two lines, and the runtime is
        // single-threaded, so the dispatch task cannot run in between: both arms
        // are ready before it observes either.
        let _ = response_tx.send(crate::cli::tui::DialogResult::Selected(0));
        cancel.cancel();

        fixture
            .join_dispatch(dispatch, query_id, "cancelled plan approval")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Planning { .. }),
            "a cancelled turn must not leave the session in the approved Executing mode; \
             mode={mode:?}, events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        let rendered = fixture.rendered_output();
        assert!(
            !rendered.contains("Plan approved"),
            "a cancelled turn must not tell the user its plan was approved; rendered={rendered:?}, \
             mode={mode:?}, results={:?}",
            fixture.results
        );
        let results = fixture.results_for(query_id);
        assert_eq!(
            results.len(),
            1,
            "a cancelled present_plan must still terminalize exactly once; mode={mode:?}, \
             events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        assert!(
            results[0]
                .2
                .as_ref()
                .is_ok_and(|text| text.contains("cancelled")),
            "the sole result must report the cancellation, not an approval; results={:?}, \
             rendered={rendered:?}",
            fixture.results
        );
    }

    /// A second turn submitted while the first is still parked on the approval
    /// dialog.  Neither turn may be lost: the approval terminalizes its own
    /// query and the later turn terminalizes its own, exactly once each.
    #[tokio::test]
    async fn test_a_second_turn_submitted_during_plan_approval_is_not_stranded() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let call = present_plan_call("present-plan-1", "The plan under review.");
        let (first_id, first_token) = fixture.begin_turn(std::slice::from_ref(&call)).await;
        let first_dispatch = fixture.spawn_dispatch(vec![call], first_id, first_token);
        let response_tx = fixture
            .await_plan_dialog("second turn during plan approval")
            .await;

        // The user types the next turn while the first dialog is still open.
        // Waiting for the second turn's own dialog pins the interleaving: both
        // turns are genuinely in flight and neither has been answered yet.
        let queued_call = present_plan_call("present-plan-2", "The turn typed while waiting.");
        let (second_id, second_token) =
            fixture.begin_turn(std::slice::from_ref(&queued_call)).await;
        let second_dispatch = fixture.spawn_dispatch(vec![queued_call], second_id, second_token);
        let second_response_tx = fixture
            .await_plan_dialog("the turn typed during approval")
            .await;

        response_tx
            .send(crate::cli::tui::DialogResult::Selected(0))
            .expect("the plan dialog receiver disappeared before the user could approve");
        second_response_tx
            .send(crate::cli::tui::DialogResult::Selected(1))
            .expect("the later turn's dialog receiver disappeared before the user could answer");
        fixture
            .join_dispatch(first_dispatch, first_id, "the approved turn")
            .await;
        fixture
            .join_dispatch(second_dispatch, second_id, "the turn typed during approval")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Executing { .. }),
            "approval must still take effect when a later turn was submitted first; mode={mode:?}, \
             events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        assert_eq!(
            fixture.results_for(first_id).len(),
            1,
            "the approved turn must terminalize exactly once; events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
        assert_eq!(
            fixture.results_for(second_id).len(),
            1,
            "the turn submitted during approval must not be stranded and must terminalize exactly \
             once; events={:?}, results={:?}",
            fixture.events,
            fixture.results
        );
    }

    /// What the dispatch loop observes once the mode changes underneath it,
    /// made explicit.  Every call in one provider batch was authored while the
    /// session was still `Planning`, so an approval part-way through does not
    /// make this loop re-gate the siblings behind it.
    ///
    /// This pins the **dispatch gate** and nothing wider.  It does not claim the
    /// batch's effective authority is unchanged: a tool that passes the gate is
    /// spawned, and the executor re-reads the mode at execution time
    /// (`src/tools/executor.rs:467`), whose block applies only while the mode is
    /// `Planning`.  A same-batch `bash` does run after an approval for exactly
    /// that reason.  `write` is refused by the gate here and is what this
    /// asserts.
    #[tokio::test]
    async fn test_mid_batch_plan_approval_does_not_widen_the_dispatch_gate() {
        let mut fixture = PlanDispatchFixture::planning().await;
        let plan_call = present_plan_call("present-plan-1", "Approve, then write.");
        let write_call = ToolUse {
            id: "write-1".into(),
            name: "write".into(),
            input: serde_json::json!({ "path": "/workspace/should-not-run", "content": "x" }),
        };
        let batch = vec![plan_call, write_call];
        let (query_id, round_token) = fixture.begin_turn(&batch).await;
        let dispatch = fixture.spawn_dispatch(batch, query_id, round_token);

        let response_tx = fixture.await_plan_dialog("mid-batch approval").await;
        response_tx
            .send(crate::cli::tui::DialogResult::Selected(0))
            .expect("the plan dialog receiver disappeared before the user could approve");
        fixture
            .join_dispatch(dispatch, query_id, "mid-batch approval")
            .await;
        fixture.drain_events();

        let mode = fixture.observed_mode().await;
        assert!(
            matches!(mode, ReplMode::Executing { .. }),
            "the approval in this batch must still take effect; mode={mode:?}, events={:?}, \
             results={:?}",
            fixture.events,
            fixture.results
        );
        let write_result = fixture
            .results
            .iter()
            .find(|(id, tool_id, _)| *id == query_id && tool_id == "write-1")
            .unwrap_or_else(|| {
                panic!(
                    "the sibling write must terminalize rather than vanish; events={:?}, \
                     results={:?}",
                    fixture.events, fixture.results
                )
            });
        assert!(
            write_result
                .2
                .as_ref()
                .err()
                .is_some_and(|message| message.contains("not allowed in planning mode")),
            "a write authored under Planning must keep that batch's authority even after a \
             mid-batch approval; write_result={write_result:?}, mode={mode:?}, events={:?}",
            fixture.events
        );
    }

    #[test]
    fn streaming_requires_both_user_opt_in_and_provider_support() {
        assert!(should_stream_responses(true, true));
        assert!(!should_stream_responses(false, true));
        assert!(!should_stream_responses(true, false));
        assert!(!should_stream_responses(false, false));
    }

    #[test]
    fn persona_is_request_local_and_owns_one_system_instruction() {
        let original = vec![crate::claude::Message::user("hello")];
        let mut request = original.clone();

        inject_persona_system_prompt(&mut request, "CUSTOM PERSONA".into());
        assert!(inject_vm_manifest(&mut request, &fallback_vm_manifest()));

        assert_eq!(original.len(), 1, "canonical conversation was not mutated");
        assert_eq!(
            request
                .iter()
                .filter(|message| message.role == "system")
                .count(),
            1
        );
        let system = request[0].text_content();
        assert!(system.contains("CUSTOM PERSONA"));
        assert_eq!(system.matches("## Finch VM wire protocol").count(), 1);
    }

    #[test]
    fn next_request_uses_newly_selected_or_reloaded_persona() {
        let mut custom = crate::config::Persona::default();
        custom.behavior.system_prompt = "CUSTOM OVERRIDE CONTENT".into();
        let reloaded = crate::config::Persona::load_builtin("analyst").unwrap();

        let mut custom_request = vec![crate::claude::Message::user("first")];
        inject_persona_system_prompt(&mut custom_request, custom.to_system_message());
        let mut reloaded_request = vec![crate::claude::Message::user("second")];
        inject_persona_system_prompt(&mut reloaded_request, reloaded.to_system_message());

        assert!(custom_request[0]
            .text_content()
            .contains("CUSTOM OVERRIDE CONTENT"));
        assert!(reloaded_request[0]
            .text_content()
            .contains(&reloaded.behavior.system_prompt));
        assert!(!reloaded_request[0]
            .text_content()
            .contains("CUSTOM OVERRIDE CONTENT"));
    }

    /// The end-of-turn refresh must not launder a partial recall into a clean
    /// one.
    ///
    /// `persist_completed_turn_memory` is the *last* writer to the memory
    /// status line, and it used to re-render the turn's recall count against a
    /// freshly sampled hydration status. On the ordinary startup path -- which
    /// #242 made normal -- hydration finishes during the turn, so the fresh
    /// sample reads `Ready` and the accurate
    /// "recalled 3 · searched at least 512 of 2048 entries" was replaced by a bare
    /// "recalled 3" at the moment the user actually read it. Carrying the
    /// observed index along with the count is what stops that (#275).
    #[tokio::test]
    async fn test_the_end_of_turn_refresh_keeps_a_partial_recall_qualified() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let memory = crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap();
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
        conversation
            .write()
            .await
            .add_user_message("a turn that recalled from a partial index".into());
        let status = StatusBar::new();
        let query_states = QueryStateManager::new();
        let query_id = query_states
            .create_query(conversation.read().await.get_messages())
            .await;

        // The index this turn's recall actually came from. The live index by
        // the time this runs is `Ready` -- that is the whole point: resampling
        // here is what produced the false line.
        persist_completed_turn_memory(
            &memory,
            &conversation,
            query_id,
            &query_states,
            "",
            &RenderedTurn::for_test("Hello from the partial index test"),
            "test-model",
            "test-brain",
            "/workspace",
            &status,
            4,
            crate::memory_status::Recall {
                count: 3,
                index: crate::memory::HydrationStatus::Loading {
                    loaded: 512,
                    total: 2048,
                },
            },
        )
        .await;

        let line = status
            .get_line(&crate::cli::status_bar::StatusLineType::MemoryContext)
            .expect("the end-of-turn refresh must write the memory line");
        assert!(
            line.contains("512 of 2048"),
            "resampled a now-complete index and presented a partial recall as whole: {line}"
        );
        assert!(line.contains('3'), "lost the recall count: {line}");
    }

    #[tokio::test]
    async fn completed_streaming_turn_populates_session_context_strip() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let memory = crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap();
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
        conversation
            .write()
            .await
            .add_user_message("test the context strip".into());
        let status = StatusBar::new();
        let query_states = QueryStateManager::new();
        let query_id = query_states
            .create_query(conversation.read().await.get_messages())
            .await;

        persist_completed_turn_memory(
            &memory,
            &conversation,
            query_id,
            &query_states,
            "",
            &RenderedTurn::for_test("Hello from the test program"),
            "test-model",
            "test-brain",
            "/workspace",
            &status,
            4,
            crate::memory_status::Recall {
                count: 2,
                index: crate::memory::HydrationStatus::Ready { nodes: 8 },
            },
        )
        .await;

        assert_eq!(memory.stats().await.unwrap().conversation_count, 2);

        // What this asserts: `persist_completed_turn_memory` indexes the string
        // it is handed, and that string survives `MemoryClassifier::is_noise`'s
        // 20-character floor. It does NOT assert the #254 fix, which is about
        // which string the two call sites in `process_query_with_tools` pass —
        // no test reaches those. A previous version added a
        // `!contains("(say")` assertion here and presented it as proof; no
        // input in this test could ever have satisfied it.
        let recalled = memory
            .query("Hello from the test program", Some(5))
            .await
            .unwrap();
        assert!(
            recalled
                .iter()
                .any(|text| text.contains("Hello from the test")),
            "the rendered result must be recallable; got {recalled:?}"
        );

        let lines = status.get_lines();
        assert!(lines.iter().any(|line| {
            line.line_type == crate::cli::status_bar::StatusLineType::MemoryContext
                && line.content == "🧠 recalled 2"
        }));
        assert!(lines.iter().any(|line| {
            matches!(
                line.line_type,
                crate::cli::status_bar::StatusLineType::ContextLine(_)
            )
        }));
    }

    #[tokio::test]
    async fn named_brain_provider_completion_waits_for_daemon_memory_projection() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let memory = crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap();
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
        conversation
            .write()
            .await
            .add_user_message("original prompt".into());
        conversation
            .write()
            .await
            .add_message(crate::claude::Message {
                role: "user".into(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: "transient tool output".into(),
                    is_error: None,
                }],
            });
        let query_states = QueryStateManager::new();
        let query_id = query_states
            .create_query(conversation.read().await.get_messages())
            .await;
        query_states
            .bind_brain_turn_provenance(
                query_id,
                super::super::query_state::BrainTurnProvenance {
                    brain_id: crate::brain::store::BrainId(uuid::Uuid::new_v4()),
                    run_id: crate::brain::store::RunId(uuid::Uuid::new_v4()),
                    request_seq: 9,
                },
            )
            .await;
        let status = StatusBar::new();

        for _ in 0..2 {
            persist_completed_turn_memory(
                &memory,
                &conversation,
                query_id,
                &query_states,
                "",
                &RenderedTurn::for_test("(say \"done\")"),
                "test-model",
                "test-brain",
                "/workspace",
                &status,
                4,
                crate::memory_status::Recall {
                    count: 2,
                    index: crate::memory::HydrationStatus::Ready { nodes: 8 },
                },
            )
            .await;
        }

        assert_eq!(memory.stats().await.unwrap().conversation_count, 0);
        let recent = memory.get_recent_conversations(10).await.unwrap();
        assert!(!recent.iter().any(|(_, text)| text == "original prompt"));
        assert!(!recent.iter().any(|(_, text)| text == "(say \"done\")"));
        assert!(!recent
            .iter()
            .any(|(_, text)| text == "transient tool output"));
    }

    struct SingleRepairGenerator {
        calls: AtomicUsize,
    }

    struct BlockingRepairGenerator {
        calls: AtomicUsize,
        started: tokio::sync::Notify,
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
                    primary_allowance_used_percent: None,
                    secondary_allowance_used_percent: None,
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

    #[async_trait::async_trait]
    impl Generator for BlockingRepairGenerator {
        async fn generate(
            &self,
            _messages: Vec<crate::claude::Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> anyhow::Result<crate::generators::GeneratorResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            std::future::pending().await
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
            "blocking-repair"
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
        assert!(text.contains("complete body of every text response is one"));
        assert!(text.contains("`ProgramSubmission`"));
        assert!(text.contains("Default to Lisp"));
        assert!(text.contains("every other valid submission is Co-Forth"));
        assert!(text.contains("already writing the active Brain's VM input"));
        assert!(text.contains("A nested CLI"));
        assert!(text.contains("process is a different runtime"));
        assert!(text.contains("submit_program` tool only when this same inference"));
        assert!(text.contains("search_word(query)"));
        assert!(text.contains("inspect_word(name)"));
        assert!(text.contains("\"Hello\" say"));
        assert!(text.contains("`s\"text\"` is equivalent"));
        assert!(messages[1].content[0]
            .as_text()
            .is_some_and(|text| text == "add two numbers"));
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

    #[test]
    fn empty_tool_continuation_reuses_the_human_query_for_the_vm_manifest() {
        let messages = vec![
            crate::claude::Message::user("hello finch"),
            crate::claude::Message::assistant(""),
        ];

        assert_eq!(vm_manifest_query(&messages, ""), "hello finch");
        assert_eq!(
            vm_manifest_query(&messages, "explicit continuation"),
            "explicit continuation"
        );
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

    #[tokio::test]
    async fn interactive_wire_scheduler_resumes_only_cooperative_yields() {
        use crate::cli::messages::{Message, MessageStatus};

        let runtime = crate::runtime::ProgramRuntime::new();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let work_unit = output.start_work_unit("VM output");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let complete = execute_direct_wire_response(
            &runtime,
            Arc::clone(&output),
            Arc::clone(&work_unit),
            event_tx.clone(),
            tokio_util::sync::CancellationToken::new(),
            "(begin (say \"one\") (yield) (say \"two\") (yield) (say \"three\"))".to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            complete.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(complete.output, "onetwothree");
        event_tx
            .send(ReplEvent::VmOutputComplete {
                output_unit: Arc::clone(&work_unit),
            })
            .unwrap();

        let mut emitted = 0;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                ReplEvent::VmEffect {
                    projection,
                    envelope,
                } => {
                    assert_eq!(work_unit.status(), MessageStatus::InProgress);
                    projection.project_envelope(envelope);
                    emitted += 1;
                }
                ReplEvent::VmOutputComplete { output_unit } => output_unit.set_complete(),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(emitted, 3, "all say events cross the UI bus");
        assert_eq!(work_unit.content(), "onetwothree");
        assert_eq!(work_unit.status(), MessageStatus::Complete);
        assert!(runtime
            .pending_typed_execution(complete.execution_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn cancelled_wire_keeps_the_execute_once_effect_prefix() {
        let runtime = crate::runtime::ProgramRuntime::new();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let work_unit = output.start_work_unit("VM output");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let execution = execute_direct_wire_response(
            &runtime,
            output,
            work_unit,
            event_tx,
            cancel.clone(),
            "(begin (say \"before\") (yield) (say \"after\"))".to_string(),
            None,
        );
        let cancel_after_prefix = async {
            let event = event_rx.recv().await.expect("first effect projection");
            assert!(matches!(event, ReplEvent::VmEffect { .. }));
            cancel.cancel();
        };
        let (outcome, ()) = tokio::join!(execution, cancel_after_prefix);
        let outcome = outcome.unwrap();

        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Cancelled
        );
        assert_eq!(outcome.output, "before");
        assert_eq!(outcome.effect_journal.len(), 1);
        assert!(matches!(
            outcome.effect_journal[0].state,
            crate::vm::EffectJournalState::Acknowledged { .. }
        ));
        assert!(runtime
            .pending_typed_execution(outcome.execution_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn named_brain_direct_wire_effect_audit_precedes_host_dispatch() {
        let task_output = tempfile::tempdir().unwrap();
        let runtime = crate::runtime::ProgramRuntime::new();
        runtime.bind_task_output_root(task_output.path()).unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Write,
                crate::vm::FileSelector::parse("${task.output}/**").unwrap(),
            ))
            .unwrap();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let work_unit = output.start_work_unit("VM output");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let (audit_tx, mut audit_rx) = tokio::sync::mpsc::unbounded_channel();
        let effect_audit = crate::server::RunnerEffectAuditControl::new(audit_tx);
        let rejection = tokio::spawn(async move {
            let crate::server::RunnerEffectAuditControlRequest::Reserve { response_tx, .. } =
                audit_rx.recv().await.expect("audit reservation request");
            response_tx
                .send(Err("direct wire audit rejected before host dispatch".into()))
                .unwrap();
        });
        let outcome = execute_direct_wire_response(
            &runtime,
            output,
            work_unit,
            event_tx,
            tokio_util::sync::CancellationToken::new(),
            "s\" blocked.txt\" task-output-path s\" secret\" bytes task-output-file-write"
                .to_string(),
            Some(effect_audit),
        )
        .await
        .unwrap();
        rejection.await.unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );
        assert!(outcome
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("direct wire audit rejected")));
        assert!(!task_output.path().join("blocked.txt").exists());
    }

    #[tokio::test]
    async fn interactive_wire_approval_resumes_the_exact_saved_program() {
        let runtime = crate::runtime::ProgramRuntime::new();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let work_unit = output.start_work_unit("VM output");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let execution = execute_direct_wire_response(
            &runtime,
            Arc::clone(&output),
            work_unit,
            event_tx,
            tokio_util::sync::CancellationToken::new(),
            "(file-read (path \"Cargo.toml\"))".to_string(),
            None,
        );
        tokio::pin!(execution);
        loop {
            tokio::select! {
                result = &mut execution => {
                    panic!("execution completed before requesting approval: {result:?}")
                }
                event = event_rx.recv() => match event.expect("interactive VM event") {
                    ReplEvent::VmApprovalNeeded { prompt, response_tx } => {
                        assert_eq!(prompt.exact.capability, crate::vm::CapabilityKind::FileRead);
                        response_tx.send(crate::vm::ApprovalChoice::AllowOnce).unwrap();
                        break;
                    }
                    ReplEvent::VmEffect { .. } => {}
                    other => panic!("unexpected event while awaiting approval: {other:?}"),
                }
            }
        }
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), execution)
            .await
            .expect("approved execution should resume")
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert!(matches!(
            outcome.values.as_slice(),
            [crate::programs::ProgramValue::Bytes(bytes)] if !bytes.is_empty()
        ));
        assert!(runtime
            .pending_typed_execution(outcome.execution_id)
            .unwrap()
            .is_none());
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.grants.grants.len(), 1);
        assert!(ledger.grants.grants[0].consumed_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn noninteractive_program_denies_new_authority_without_opening_ui() {
        let runtime = crate::runtime::ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let submission =
            direct_wire_submission(&runtime, "(file-read (path \"Cargo.toml\"))".to_string())
                .unwrap();
        let suspended = runtime
            .submit_typed_only_with_grant_ceiling(submission, crate::vm::EffectSet::pure())
            .await
            .unwrap();
        assert_eq!(
            suspended.status,
            crate::runtime::outcome::ExecutionStatus::AuthorizationRequired
        );
        assert_eq!(suspended.approval_prompts.len(), 1);

        let denied = resume_noninteractive_boundaries(&runtime, suspended)
            .await
            .unwrap();
        assert_eq!(
            denied.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );
        assert!(runtime
            .pending_typed_execution(denied.execution_id)
            .unwrap()
            .is_none());
        assert_eq!(runtime.capability_ledger().unwrap().grants.grants.len(), 1);
        assert!(denied
            .effect_journal
            .iter()
            .any(|entry| matches!(entry.state, crate::vm::EffectJournalState::Denied { .. })));
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

    #[test]
    fn every_nonempty_text_response_streams_as_candidate_program_source() {
        assert!(has_streamed_wire_source("(say \"hello\")"));
        assert!(has_streamed_wire_source("\"hello\" say"));
        assert!(has_streamed_wire_source("[ 1 2 3 ]"));
        assert!(has_streamed_wire_source("{ name: \"Finch\" }"));
        assert!(has_streamed_wire_source("-6 factorial"));

        // Invalid submissions stay visible for diagnosis instead of being
        // silently treated as an assistant-prose side channel.
        assert!(has_streamed_wire_source("#!/bin/bash"));
        assert!(has_streamed_wire_source("Sure, I will inspect that."));
        assert!(has_streamed_wire_source("```lisp"));
        assert!(!has_streamed_wire_source("  \n\t"));
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
        let metrics_dir = tempfile::tempdir().unwrap();
        let metrics = crate::metrics::MetricsLogger::new(metrics_dir.path().to_path_buf()).unwrap();

        let execution = execute_wire_with_single_repair(
            &runtime,
            Arc::clone(&output),
            event_tx,
            tokio_util::sync::CancellationToken::new(),
            generator.clone(),
            &[crate::claude::Message::user("reply")],
            source.clone(),
            Some(&metrics),
            None,
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
            if !projection.project_envelope(envelope).is_empty() {
                projected += 1;
            }
        }

        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(projected, 1, "the repaired say must cross the event bus");
        assert_eq!(execution.source_for_history, "(say \"repaired\")");
        assert_eq!(execution.response, "repaired");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let recorded = metrics.read_wire_metrics(&today).unwrap();
        assert_eq!(recorded.len(), 1);
        assert!(!recorded[0].first_pass_valid);
        assert_eq!(
            recorded[0].failure_class,
            Some(crate::metrics::WireFailureClass::MarkdownFence)
        );
        assert!(recorded[0].repair_attempted);
        assert!(recorded[0].repaired_successfully);
        assert!(!recorded[0].terminal_failure);
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

    #[tokio::test]
    async fn named_brain_effect_audit_cancel_before_repair_never_invokes_provider() {
        let runtime = crate::runtime::ProgramRuntime::new();
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let generator = Arc::new(SingleRepairGenerator {
            calls: AtomicUsize::new(0),
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let source = raw_wire_source("```lisp\n(say \"must not run\")\n```");

        let execution = execute_wire_with_single_repair(
            &runtime,
            output,
            event_tx,
            cancel,
            generator.clone(),
            &[crate::claude::Message::user("reply")],
            source.clone(),
            None,
            None,
        )
        .await;

        assert_eq!(generator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(execution.source_for_history, source);
        assert!(execution.response.contains("E-WIRE-002"));
    }

    #[tokio::test]
    async fn named_brain_effect_audit_cancel_drops_inflight_repair_without_continuation() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        let output = Arc::new(OutputManager::default());
        output.disable_stdout();
        let generator = Arc::new(BlockingRepairGenerator {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        let source = raw_wire_source("```lisp\n(say \"must not run\")\n```");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let execution = {
            let runtime = Arc::clone(&runtime);
            let output = Arc::clone(&output);
            let generator = Arc::clone(&generator);
            let cancel = cancel.clone();
            let source = source.clone();
            tokio::spawn(async move {
                execute_wire_with_single_repair(
                    runtime.as_ref(),
                    output,
                    event_tx,
                    cancel,
                    generator,
                    &[crate::claude::Message::user("reply")],
                    source,
                    None,
                    None,
                )
                .await
            })
        };
        generator.started.notified().await;
        cancel.cancel();
        let execution = tokio::time::timeout(std::time::Duration::from_secs(1), execution)
            .await
            .expect("cancellation must abort repair generation promptly")
            .unwrap();

        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(execution.source_for_history, source);
        assert!(execution.response.contains("E-WIRE-002"));
        assert!(event_rx.try_recv().is_err(), "no repaired VM continuation");
        assert!(output.get_messages().iter().all(|message| !message
            .format(&crate::config::ColorScheme::default())
            .contains("VM program repair")));
    }

    #[test]
    fn wire_repair_prompt_preserves_the_rejected_program_and_requires_raw_source() {
        let messages = vec![crate::claude::Message::user("say hello")];
        let repair = wire_repair_messages(&messages, "Hello!", "E-LINK-002: unknown word");
        assert_eq!(repair.len(), 3);
        assert_eq!(repair[1].role, "assistant");
        assert_eq!(repair[1].content[0].as_text(), Some("Hello!"));
        let prompt = repair[2].content[0].as_text().unwrap();
        assert!(prompt.contains("It was forth; repair it as forth"));
        assert!(prompt.contains("exactly one complete raw Finch forth ProgramSubmission"));
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

    #[test]
    fn completed_history_replaces_text_without_reordering_opaque_continuation() {
        let blocks = vec![
            ContentBlock::opaque_reasoning("opaque-before"),
            ContentBlock::text("wire-before-repair"),
            ContentBlock::opaque_reasoning("opaque-after"),
        ];
        let content = history_content_with_source(&blocks, "wire-after-repair".into()).unwrap();
        assert!(matches!(
            content.as_slice(),
            [
                ContentBlock::OpaqueReasoning { encrypted_content: before },
                ContentBlock::Text { text },
                ContentBlock::OpaqueReasoning { encrypted_content: after },
            ] if before == "opaque-before" && text == "wire-after-repair" && after == "opaque-after"
        ));

        let multiple = vec![
            ContentBlock::text("first"),
            ContentBlock::opaque_reasoning("opaque-middle"),
            ContentBlock::text("second"),
        ];
        assert_eq!(
            serde_json::to_value(
                history_content_with_source(&multiple, "firstsecond".into()).unwrap()
            )
            .unwrap(),
            serde_json::to_value(&multiple).unwrap()
        );
        assert!(history_content_with_source(&multiple, "rewritten".into()).is_err());
    }
}
