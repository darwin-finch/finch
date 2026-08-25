//! Main `EventLoop` — orchestrates user input, query dispatch, and TUI rendering.
//!
//! The event loop runs a `select!` over three streams:
//!
//! * **User input** from `spawn_input_task` (keystrokes, submit, Ctrl+C).
//! * **Query events** from the `ReplEvent` mpsc channel (streaming chunks,
//!   tool results, approval requests, brain messages).
//! * **Render tick** (~100ms) — flushes buffered output to the TUI.
//!
//! ## Submodules used
//! * `plan_handler` — intercepts `PresentPlan` / `AskUserQuestion` tool calls.
//! * `tool_display` — formats tool output for display rows.
//! * `tool_execution` — concurrent tool dispatch with approval gating.
//! * `query_state` — per-query state machine (pending → streaming → done).

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::style::Stylize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::claude::ContentBlock;
use crate::cli::commands::{format_help, Command};
use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::{OutputManager, VmOutputProjection};
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::{spawn_input_task, TuiRenderer};
use crate::feedback::{FeedbackEntry, FeedbackLogger, FeedbackRating};
use crate::generators::Generator;
use crate::local::LocalGenerator;
use crate::memory::NeuralEmbeddingEngine;
use crate::models::bootstrap::GeneratorState;
use crate::models::tokenizer::TextTokenizer;
use crate::router::Router;
use crate::session::diff_store::DiffStore;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::ToolDefinition;

use super::events::{LlmRequest, ReplEvent};
use super::llm_loop::LlmLoop;
use super::model_selection::{activate_local_when_ready, LocalActivationOutcome, ModelSelection};
use super::query_processor::{refresh_context_strip, ActiveToolUsesMap};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_display::tool_result_to_display;
use super::tool_execution::ToolExecutionCoordinator;

// refresh_context_strip, dispatch_tool_uses, process_query_with_tools,
// ActiveToolUsesMap, and apply_sliding_window live in query_processor.rs.

type ToolResultsMap = Arc<RwLock<std::collections::HashMap<Uuid, Vec<(String, Result<String>)>>>>;
type PendingApprovalsMap = Arc<
    RwLock<
        std::collections::HashMap<
            Uuid,
            (
                crate::tools::types::ToolUse,
                tokio::sync::oneshot::Sender<super::events::ConfirmationResult>,
            ),
        >,
    >,
>;

pub(crate) fn resolve_provider_profile(
    providers: &[crate::config::ProviderEntry],
    selector: &str,
) -> std::result::Result<usize, String> {
    if let Ok(number) = selector.parse::<usize>() {
        return if number > 0 && number <= providers.len() {
            Ok(number - 1)
        } else {
            Err(format!(
                "Invalid model number: {number}. Use 1-{}",
                providers.len()
            ))
        };
    }

    let selector = selector.trim();
    let exact: Vec<usize> = providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.profile_name().eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    if exact.len() > 1 {
        return Err(format!(
            "Model name '{selector}' is ambiguous; give these profiles unique names in config"
        ));
    }

    let by_type: Vec<usize> = providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.provider_type().eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect();
    match by_type.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!(
            "Unknown model profile '{selector}'. Run /model list to see configured profiles"
        )),
        _ => Err(format!(
            "Provider type '{selector}' matches multiple profiles; select one by name or number"
        )),
    }
}

/// Continuation data for a poset run that is waiting for user confirmation.
struct PendingPosetRun {
    generator: Arc<dyn crate::generators::Generator>,
    /// Immutable snapshot of exactly the plan shown in the approval dialog.
    /// Subsequent edits to the live plan cannot change the approved execution.
    poset: crate::poset::Poset,
    event_tx: tokio::sync::mpsc::UnboundedSender<ReplEvent>,
}

/// Main event loop for concurrent REPL
#[allow(dead_code)]
pub struct EventLoop {
    /// Channel for receiving events
    event_rx: mpsc::UnboundedReceiver<ReplEvent>,
    /// Channel for sending events
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Channel for receiving user input events
    input_rx: mpsc::UnboundedReceiver<crate::cli::tui::InputEvent>,

    /// Shared conversation history
    conversation: Arc<RwLock<ConversationHistory>>,

    /// Query state manager
    query_states: Arc<QueryStateManager>,

    /// Active and pending session model state.
    model_selection: ModelSelection,

    /// Qwen generator (unified interface)
    qwen_gen: Arc<dyn Generator>,

    /// Available providers from config (for /provider list + switching)
    available_providers: Vec<crate::config::ProviderEntry>,

    /// HTTP daemon client used for local-model status and generation.
    daemon_client: Option<Arc<crate::client::DaemonClient>>,

    /// Router for deciding between generators
    router: Arc<Router>,

    /// Generator state for bootstrap tracking
    generator_state: Arc<RwLock<GeneratorState>>,

    /// Tool definitions for Claude API
    tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>,

    /// TUI renderer
    tui_renderer: Arc<Mutex<TuiRenderer>>,

    /// Output manager
    output_manager: Arc<OutputManager>,

    /// Status bar
    status_bar: Arc<StatusBar>,

    /// Whether streaming is enabled
    streaming_enabled: bool,

    /// Tool execution coordinator
    tool_coordinator: ToolExecutionCoordinator,

    /// The frontend-owned VM runtime. It is used only to resume an existing,
    /// verified portable effect; the daemon never acquires workspace authority
    /// through this field.
    program_runtime: Arc<crate::runtime::ProgramRuntime>,

    /// Tool results collected per query (query_id -> Vec<(tool_id, result)>)
    tool_results: ToolResultsMap,

    /// Currently active query ID (for cancellation)
    active_query_id: Arc<RwLock<Option<Uuid>>>,

    /// User turns submitted while a provider/VM turn is active.  The legacy
    /// code overwrote `active_query_id`, leaving the earlier turn unable to
    /// clear itself and making the UI appear frozen.  Queue textual turns so
    /// the single shared conversation and VM revision advance in order.
    pending_queries: std::collections::VecDeque<(String, bool, bool)>,

    /// Full turns requested by the daemon for the Brain whose runner lease
    /// this frontend owns. Completion is correlated to the ordinary query ID
    /// so tool continuations use the exact same pipeline as local turns.
    pending_named_brain_turns: std::collections::HashMap<Uuid, PendingNamedBrainTurn>,

    /// Cancellation controls for typed programs delegated by the Brain daemon.
    pending_named_brain_programs:
        std::collections::HashMap<crate::brain::shared::RunId, tokio_util::sync::CancellationToken>,

    /// Source/output already rendered while this frontend serviced its home
    /// Brain callback. Matching canonical events advance this marker without
    /// drawing a second copy in the runner console.
    local_brain_projections: std::collections::VecDeque<LocalBrainProjection>,

    /// Canonical tool calls replay into one grouped unit per Brain turn.
    remote_brain_tool_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
    remote_brain_tool_rows: std::collections::HashMap<String, usize>,
    remote_brain_approval_rows: std::collections::HashMap<String, usize>,
    queued_remote_brain_approvals: std::collections::VecDeque<RemoteBrainApproval>,
    active_remote_brain_approval: Option<RemoteBrainApproval>,

    /// Pending tool approval requests (query_id -> (tool_use, response_tx))
    pending_approvals: PendingApprovalsMap,

    /// Structured approval continuation for an exact typed-VM capability
    /// request. The choices mirror the displayed rows by index.
    pending_vm_approval: Option<PendingVmApproval>,

    /// IPC client — Cap'n Proto channel to the daemon.
    /// Must live inside a tokio LocalSet (capnp-rpc !Send).
    ipc_client: Option<crate::ipc::IpcClient>,

    /// REPL mode (Normal, Planning, Executing)
    mode: Arc<RwLock<ReplMode>>,

    /// Plan content storage (for PresentPlan tool)
    plan_content: Arc<RwLock<Option<String>>>,

    /// Memory tree console for tree-structured conversation view
    memtree_console: Arc<RwLock<crate::cli::memtree_console::MemTreeConsole>>,

    /// Event handler for translating REPL events to tree operations
    memtree_handler: Arc<tokio::sync::Mutex<crate::cli::memtree_console::EventHandler>>,

    /// Current view mode (List or Tree)
    view_mode: Arc<RwLock<ViewMode>>,

    /// Active tool calls: tool_id -> (tool_name, input, work_unit, row_idx)
    /// All tools in one generation turn share the same WorkUnit; each
    /// tool occupies one row identified by its index.
    active_tool_uses: ActiveToolUsesMap,

    /// Feedback logger — writes rated responses to ~/.finch/feedback.jsonl
    feedback_logger: Option<FeedbackLogger>,

    /// Metrics logger — reads from ~/.finch/metrics/ for /metrics command
    metrics_logger: Option<Arc<crate::metrics::MetricsLogger>>,

    /// Memory system for semantic recall across sessions
    memory_system: Option<Arc<crate::memory::MemorySystem>>,

    /// Human-readable label for this session (e.g. "swift-falcon")
    session_label: String,

    /// Human participant identity shown on attachments and runner leases.
    /// This is deliberately separate from the Brain's name and opaque lease IDs.
    participant_subject: String,

    /// Ephemeral identity of this exact frontend execution context. Human
    /// participant identity is not precise enough for an addressed handoff
    /// between two consoles owned by the same user.
    runner_subject: String,

    /// Stable UUID for this session — assigned at startup, printed on exit.
    session_uuid: Uuid,

    /// Working directory at startup (for terminal title)
    cwd: String,

    /// Total number of status-strip lines (🧠 + context summaries).
    /// Comes from config.features.memory_context_lines (default 4).
    context_lines: usize,

    /// Maximum number of recent messages sent verbatim to the provider.
    /// Set to 0 to disable windowing. From config.features.max_verbatim_messages.
    max_verbatim_messages: usize,

    /// Number of MemTree results recalled and injected per query.
    /// From config.features.context_recall_k.
    context_recall_k: usize,

    /// Session task list shared with TodoWrite / TodoRead tools
    todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,

    /// Whether to summarise dropped messages (Infinite Context Phase 2).
    /// From config.features.enable_summarization.
    enable_summarization: bool,

    /// Whether sliding-window auto-compaction is enabled.
    /// From config.features.auto_compact_enabled. Default: true.
    auto_compact_enabled: bool,

    /// Explicit destination for prompts and VM programs while attached.
    /// This is singular by design: host effects are never broadcast.
    active_remote_brain: Option<crate::brain::remote::AttachedBrainClient>,

    /// Durable attachment to this console's home Brain. Ordinary input uses
    /// this attachment whenever no foreign Brain is selected, so the runner
    /// console and remote drivers project the same canonical event log.
    home_brain: Option<crate::brain::remote::AttachedBrainClient>,

    /// Whether this frontend currently holds the daemon-issued lease for its
    /// home Brain. The UI never infers runner status from local process role.
    home_runner_lease_active: bool,
    home_runner_lease_id: Option<crate::brain::shared::RunnerLeaseId>,
    /// Exact Brain currently served by this frontend's ProgramRuntime. This
    /// starts as the home Brain but may change through an addressed handoff.
    runner_brain: Option<String>,
    /// Invalidates background renewal tasks when runner ownership moves.
    runner_renewal_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// Last runner-registration failure shown to the user. Lease renewal is
    /// periodic, so identical transport failures must not spam scrollback.
    last_home_runner_error: Option<String>,

    /// Base URL of the local daemon (e.g. "http://127.0.0.1:8000").
    /// Used by the cross-machine relay poller.
    daemon_base_url: Option<String>,

    /// Oneshot sender for a dialog shown via `ReplEvent::ShowDialog`.
    /// The render tick delivers `pending_dialog_result` here when the dialog completes.
    pending_dialog_tx: Option<tokio::sync::oneshot::Sender<crate::cli::tui::DialogResult>>,

    /// Data for a pending Co-Forth poset run that is waiting on a confirmation dialog.
    pending_poset_run: Option<PendingPosetRun>,

    /// Per-query tool call history: query_id -> set of "tool_name:input_json" strings.
    /// Used to detect infinite loops (same tool called with same args multiple times).
    tool_call_history:
        Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,

    /// Execution graph for the current (or most recent) query.
    current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>,

    /// Co-Forth shared stack: items pushed by the user (text) or by the AI (Push tool).
    /// Arc<Mutex> so the tool executor can write to it during generation.
    stack: Arc<tokio::sync::Mutex<Vec<String>>>,

    /// Co-Forth poset VM — partially-ordered task graph with 3D renderer.
    poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>,

    /// The Co-Forth word that was popped when entering plan mode.
    /// Stored so the user can re-plan without losing the word.
    plan_word: Option<String>,

    /// Local transport for the legacy diff-review event store.
    peer_tx: tokio::sync::broadcast::Sender<crate::session::SessionEvent>,

    /// In-memory store of pending diff proposals.
    diff_store: DiffStore,

    /// Receiver for local Diff/DiffEdit/DiffAccept/DiffReject events.
    peer_session_rx: tokio::sync::broadcast::Receiver<crate::session::SessionEvent>,

    // ── LLM worker loop channel ───────────────────────────────────────────
    /// Send LLM requests to the worker loop.
    llm_tx: mpsc::UnboundedSender<LlmRequest>,
    /// Receiver held until `run()` hands it off to `LlmLoop`.
    llm_rx: Option<mpsc::UnboundedReceiver<LlmRequest>>,
}

/// View mode for the REPL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Traditional list view (current scrollback)
    List,
    /// Tree-structured conversation view
    Tree,
}

fn local_participant_subject() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let machine = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "machine".into());
    participant_subject_from(&user, &machine)
}

fn participant_subject_from(user: &str, machine: &str) -> String {
    let user = user.trim();
    let machine = machine.trim();
    let value = format!(
        "{}@{}",
        if user.is_empty() { "user" } else { user },
        if machine.is_empty() { "machine" } else { machine }
    );
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect()
}

fn runner_subject_from(participant: &str, frontend_id: Uuid) -> String {
    let suffix = format!("/frontend-{}", &frontend_id.to_string()[..8]);
    let keep = 128usize.saturating_sub(suffix.len());
    let mut base = participant.chars().take(keep).collect::<String>();
    base.push_str(&suffix);
    base
}

fn approval_audience_summary(
    audience: &crate::brain::shared::BrainApprovalAudience,
) -> String {
    format!(
        "Brain: {} ({})\nApproval audience: {} ({:?}, attachment {})\nEnvironment generation: {}",
        audience.brain,
        audience.brain_id.0,
        audience.subject,
        audience.role,
        audience.attachment_id.0,
        audience.environment_generation
    )
}

fn vm_approval_choices(prompt: &crate::vm::ApprovalPrompt) -> Vec<crate::vm::ApprovalChoice> {
    let mut choices = vec![crate::vm::ApprovalChoice::AllowOnce];
    if !prompt.request.agent_ancestry.is_empty() {
        choices.push(crate::vm::ApprovalChoice::AllowTask);
    }
    choices.push(crate::vm::ApprovalChoice::AllowSession);
    choices.push(crate::vm::ApprovalChoice::AllowProjectExact);
    if let Some(requirement) = prompt.suggested_patterns.first().cloned() {
        choices.push(crate::vm::ApprovalChoice::AllowProjectPattern { requirement });
    }
    choices.push(crate::vm::ApprovalChoice::Deny);
    choices
}

fn vm_approval_dialog(
    prompt: &crate::vm::ApprovalPrompt,
    audience: Option<&crate::brain::shared::BrainApprovalAudience>,
    runtime: &crate::runtime::ProgramRuntime,
) -> crate::cli::tui::Dialog {
    use crate::cli::tui::{Dialog, DialogOption};

    let choices = vm_approval_choices(prompt);
    let options = choices
        .iter()
        .map(|choice| match choice {
            crate::vm::ApprovalChoice::AllowOnce => DialogOption::with_description(
                "Allow once",
                "Resume only this exact pending effect",
            ),
            crate::vm::ApprovalChoice::AllowTask => DialogOption::with_description(
                "Allow for task",
                "Reuse only within this child task",
            ),
            crate::vm::ApprovalChoice::AllowSession => DialogOption::with_description(
                "Allow for session",
                "Reuse in this resumable Finch session",
            ),
            crate::vm::ApprovalChoice::AllowProjectExact => DialogOption::with_description(
                "Allow for project",
                "Reuse this exact capability in the current project",
            ),
            crate::vm::ApprovalChoice::AllowProjectPattern { .. } => {
                DialogOption::with_description(
                    "Allow project pattern",
                    "Reuse the displayed narrowed pattern in this project",
                )
            }
            crate::vm::ApprovalChoice::AllowGlobal => DialogOption::with_description(
                "Allow globally",
                "Reuse this capability outside the current project",
            ),
            crate::vm::ApprovalChoice::Deny => DialogOption::new("Deny"),
        })
        .collect();
    let exact = serde_json::to_string_pretty(&prompt.exact)
        .unwrap_or_else(|_| format!("{:?}", prompt.exact));
    let availability = runtime.capability_availability(&prompt.exact);
    let warning = if prompt.broad_scope_warning {
        "\n\nWarning: this request covers a broad resource scope."
    } else {
        ""
    };
    let audience = audience
        .map(|audience| format!("\n\n{}", approval_audience_summary(audience)))
        .unwrap_or_default();
    Dialog::select("Finch VM capability request", options).with_body(format!(
        "Reason: {}\nHost availability: {:?}\n\nRequired capability:\n{}{}{}",
        prompt.request.reason, availability, exact, warning, audience
    ))
}

/// Application-owned data extracted from one verified, suspended
/// `proposal-open` effect. The effect handle—not source text—is the authority
/// to resume this exact VM frame.
struct DeferredProposal {
    handle: crate::runtime::VmEffectHandle,
    language: String,
    intent: String,
    source: String,
}

/// Exact approval continuation returned by a provider-native
/// `submit_program` call. The application retains the prompt rather than
/// asking the model to resubmit or broaden its source declaration.
struct DeferredVmApproval {
    prompt: crate::vm::ApprovalPrompt,
}

struct PendingNamedBrainTurn {
    run_id: crate::brain::shared::RunId,
    response_tx: tokio::sync::oneshot::Sender<
        std::result::Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError>,
    >,
    /// Exact lifecycle order observed by this frontend while servicing the
    /// delegated turn. The daemon persists these as canonical Brain events.
    turn_events: Vec<crate::server::RunnerTurnEvent>,
    approval_audience: crate::brain::shared::BrainApprovalAudience,
    approval_tx: Option<
        tokio::sync::mpsc::UnboundedSender<crate::server::RunnerApprovalRequest>,
    >,
}

#[derive(Clone)]
struct RemoteBrainApproval {
    client: crate::brain::remote::AttachedBrainClient,
    request_seq: u64,
    approval_id: String,
    audience: crate::brain::shared::BrainApprovalAudience,
    kind: RemoteBrainApprovalKind,
}

#[derive(Clone)]
enum RemoteBrainApprovalKind {
    Tool(crate::tools::types::ToolUse),
    Vm {
        prompt: crate::vm::ApprovalPrompt,
        choices: Vec<crate::vm::ApprovalChoice>,
    },
}

struct PendingVmApproval {
    response_tx: tokio::sync::oneshot::Sender<crate::vm::ApprovalChoice>,
    choices: Vec<crate::vm::ApprovalChoice>,
    query_id: Option<Uuid>,
    approval_id: String,
}

struct LocalBrainProjection {
    source: String,
    output: String,
    tool_ids: std::collections::HashSet<String>,
    approval_ids: std::collections::HashSet<String>,
    program_seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalProjectionMatch {
    None,
    Suppress,
    SuppressAndComplete,
}

impl LocalBrainProjection {
    fn observe(&mut self, event: &crate::brain::shared::BrainEvent) -> LocalProjectionMatch {
        match &event.kind {
            crate::brain::shared::BrainEventKind::ToolCall { tool_id, .. }
            | crate::brain::shared::BrainEventKind::ToolResult { tool_id, .. }
                if self.tool_ids.contains(tool_id) =>
            {
                LocalProjectionMatch::Suppress
            }
            crate::brain::shared::BrainEventKind::ApprovalRequested { approval_id, .. }
            | crate::brain::shared::BrainEventKind::ApprovalDecided { approval_id, .. }
                if self.approval_ids.contains(approval_id) =>
            {
                LocalProjectionMatch::Suppress
            }
            crate::brain::shared::BrainEventKind::Program { source, .. }
                if event.sender == "provider"
                    && self.program_seq.is_none()
                    && self.source == *source =>
            {
                self.program_seq = Some(event.seq);
                LocalProjectionMatch::Suppress
            }
            crate::brain::shared::BrainEventKind::Result {
                request_seq,
                output,
                error,
            } if self.program_seq == Some(*request_seq)
                && error.is_none()
                && self.output == *output =>
            {
                LocalProjectionMatch::SuppressAndComplete
            }
            _ => LocalProjectionMatch::None,
        }
    }
}

fn deferred_vm_approval_from_tool_result(
    result: &anyhow::Result<String>,
) -> Option<DeferredVmApproval> {
    let content = result.as_ref().ok()?;
    let outcome: crate::runtime::outcome::ExecutionOutcome = serde_json::from_str(content).ok()?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::AuthorizationRequired {
        return None;
    }
    Some(DeferredVmApproval {
        prompt: outcome.approval_prompts.into_iter().next()?,
    })
}

fn deferred_proposal_from_tool_result(
    result: &anyhow::Result<String>,
) -> Option<DeferredProposal> {
    let content = result.as_ref().ok()?;
    let outcome: crate::runtime::outcome::ExecutionOutcome = serde_json::from_str(content).ok()?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::Suspended {
        return None;
    }
    let effect = outcome.vm_side_effects.iter().rev().find(|effect| {
        effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
            && matches!(effect.event, crate::vm::HostSideEffect::Request { .. })
    })?;
    let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
        return None;
    };
    let [
        crate::vm::TypedValue::String(language),
        crate::vm::TypedValue::String(intent),
        crate::vm::TypedValue::String(source),
    ] = arguments.as_slice()
    else {
        return None;
    };
    Some(DeferredProposal {
        handle: crate::runtime::VmEffectHandle {
            execution_id: outcome.execution_id,
            sequence: effect.sequence,
        },
        language: language.clone(),
        intent: intent.clone(),
        source: source.clone(),
    })
}

fn proposal_resume_values(
    decision: crate::tools::implementations::propose::ProposalDecision,
) -> Vec<crate::vm::TypedValue> {
    let inner_type = crate::vm::Type::Result(
        Box::new(crate::vm::Type::String),
        Box::new(crate::vm::Type::String),
    );
    let value = match decision {
        crate::tools::implementations::propose::ProposalDecision::Execute { source } => {
            Some(Box::new(crate::vm::TypedValue::Result {
                ok_type: crate::vm::Type::String,
                error_type: crate::vm::Type::String,
                is_ok: true,
                value: Box::new(crate::vm::TypedValue::String(source)),
            }))
        }
        crate::tools::implementations::propose::ProposalDecision::Chat { context } => {
            Some(Box::new(crate::vm::TypedValue::Result {
                ok_type: crate::vm::Type::String,
                error_type: crate::vm::Type::String,
                is_ok: false,
                value: Box::new(crate::vm::TypedValue::String(context)),
            }))
        }
        crate::tools::implementations::propose::ProposalDecision::Cancel => None,
    };
    vec![crate::vm::TypedValue::Option { inner_type, value }]
}

/// Complete one application-owned proposal decision by resuming the exact VM
/// effect that opened it.  This deliberately accepts a decision rather than
/// source text: editing is outside the VM, while the VM only observes the
/// typed accepted/chat/cancel result correlated to its effect handle.
async fn resume_deferred_proposal(
    runtime: &crate::runtime::ProgramRuntime,
    proposal: &DeferredProposal,
    decision: crate::tools::implementations::propose::ProposalDecision,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    runtime
        .resume_typed_execution_with_effect_result(
            proposal.handle.execution_id,
            proposal.handle.sequence,
            proposal_resume_values(decision),
        )
        .await
}

#[cfg(test)]
mod deferred_proposal_tests {
    use super::*;

    #[tokio::test]
    async fn extracts_the_exact_suspended_proposal_handle() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("proposal-test.lisp".into()),
                    source: "(proposal-open \"python\" \"inspect artifact\" \"print('ok')\")"
                        .into(),
                    intent: "proposal test".into(),
                    effect: crate::programs::ExecutionEffect::ExternalWrite,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let proposal = deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");
        assert_eq!(proposal.handle.execution_id, outcome.execution_id);
        assert_eq!(proposal.handle.sequence, 0);
        assert_eq!(proposal.language, "python");
        assert_eq!(proposal.intent, "inspect artifact");
        assert_eq!(proposal.source, "print('ok')");
    }

    #[tokio::test]
    async fn extracts_the_exact_submit_program_approval_prompt() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        let outcome = runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("approval-tool-test.lisp".into()),
                    source: "(file-read (path \"Cargo.toml\"))".into(),
                    intent: "read the manifest".into(),
                    effect: crate::programs::ExecutionEffect::WorkspaceRead,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let approval = deferred_vm_approval_from_tool_result(&Ok(
            serde_json::to_string(&outcome).unwrap(),
        ))
        .expect("authorization-required tool outcome");

        assert_eq!(approval.prompt.request.execution_id, outcome.execution_id);
        assert_eq!(approval.prompt.request.effect_sequence, Some(0));
        assert_eq!(
            approval.prompt.exact.capability,
            crate::vm::CapabilityKind::FileRead
        );
    }

    #[tokio::test]
    async fn proposal_decision_resumes_the_saved_effect_without_replaying_source() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("proposal-test.lisp".into()),
                    source: "(proposal-open \"python\" \"inspect artifact\" \"print('original')\")"
                        .into(),
                    intent: "proposal test".into(),
                    effect: crate::programs::ExecutionEffect::ExternalWrite,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let proposal = deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");

        let completed = resume_deferred_proposal(
            runtime.as_ref(),
            &proposal,
            crate::tools::implementations::propose::ProposalDecision::Chat {
                context: "Please explain the artifact first.".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            completed.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(completed.vm_side_effects.len(), 1);
        assert!(matches!(
            completed.values.as_slice(),
            [crate::programs::ProgramValue::Option(Some(value))]
                if matches!(value.as_ref(), crate::programs::ProgramValue::Result { ok: false, value }
                    if matches!(value.as_ref(), crate::programs::ProgramValue::String(context)
                        if context == "Please explain the artifact first."))
        ));
        assert!(runtime
            .pending_typed_execution(proposal.handle.execution_id)
            .unwrap()
            .is_none());
    }
}

impl EventLoop {
    /// Create a new event loop with unified generators
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Arc<RwLock<ConversationHistory>>,
        _cloud_gen: Arc<dyn Generator>,
        qwen_gen: Arc<dyn Generator>,
        router: Arc<Router>,
        generator_state: Arc<RwLock<GeneratorState>>,
        tool_definitions: Vec<ToolDefinition>,
        tool_executor: Arc<Mutex<ToolExecutor>>,
        program_runtime: Arc<crate::runtime::ProgramRuntime>,
        tui_renderer: TuiRenderer,
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        streaming_enabled: bool,
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
        ipc_client: Option<crate::ipc::IpcClient>,
        mode: Arc<RwLock<ReplMode>>,
        memory_system: Option<Arc<crate::memory::MemorySystem>>,
        session_label: String,
        available_providers: Vec<crate::config::ProviderEntry>,
        active_provider_index: usize,
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
        context_lines: usize,
        max_verbatim_messages: usize,
        context_recall_k: usize,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
        enable_summarization: bool,
        auto_compact_enabled: bool,
        daemon_base_url: Option<String>,
        provider_resolver: crate::runtime::scheduler::ProviderResolver,
        agent_scheduler: Arc<crate::runtime::scheduler::AgentScheduler>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (llm_tx, llm_rx) = mpsc::unbounded_channel::<LlmRequest>();

        let mut agent_events = agent_scheduler.subscribe();
        let agent_event_tx = event_tx.clone();
        tokio::spawn(async move {
            loop {
                match agent_events.recv().await {
                    Ok(event) => {
                        if agent_event_tx
                            .send(ReplEvent::AgentLifecycle(event))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Create Co-Forth shared stack before TUI so both hold the same Arc.
        let stack: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        // Create Co-Forth poset VM before TUI so both hold the same Arc.
        let poset: Arc<tokio::sync::Mutex<crate::poset::Poset>> =
            Arc::new(tokio::sync::Mutex::new(crate::poset::Poset::new()));

        // Wire todo list, stack, and poset into TUI renderer before wrapping in Arc<Mutex>
        let mut tui_renderer = tui_renderer;
        tui_renderer.set_todo_list(Arc::clone(&todo_list));
        tui_renderer.set_stack(Arc::clone(&stack));
        tui_renderer.set_poset(Arc::clone(&poset));
        // Wrap TUI in Arc<Mutex> for shared access
        let tui_renderer = Arc::new(Mutex::new(tui_renderer));

        // Spawn quit watcher — a dedicated task that receives Cap'n Proto binary
        // ControlMessage { quit } and exits the process immediately.
        // This runs independently of the event loop so /quit always works even
        // when the loop is blocked mid-streaming or mid-tool execution.
        let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = quit_rx.recv().await {
                let mut cursor = std::io::Cursor::new(bytes);
                let ok = capnp::serialize::read_message(
                    &mut cursor,
                    capnp::message::ReaderOptions::new(),
                )
                .and_then(|reader| {
                    reader
                        .get_root::<crate::finch_ipc_capnp::control_message::Reader>()
                        .map(|ctrl| {
                            matches!(
                                ctrl.which(),
                                Ok(crate::finch_ipc_capnp::control_message::Which::Quit(_))
                            )
                        })
                })
                .unwrap_or(false);

                if ok {
                    crate::cli::tui::emergency_restore_terminal();
                    std::process::exit(0);
                }
            }
        });

        // Spawn input handler task
        let input_rx = spawn_input_task(Arc::clone(&tui_renderer), quit_tx);

        // Initialize plan content storage
        let plan_content = Arc::new(RwLock::new(None));

        // Create the tool coordinator. Tool results are conversation events;
        // they never mutate the unrelated legacy semiotic stack.
        let tool_coordinator = ToolExecutionCoordinator::new(
            event_tx.clone(),
            Arc::clone(&tool_executor),
            Arc::clone(&output_manager),
            Arc::clone(&conversation),
            Arc::clone(&local_generator),
            Arc::clone(&tokenizer),
            Arc::clone(&mode),
            Arc::clone(&plan_content),
        )
        .with_poset(Arc::clone(&poset));

        // Initialize memtree console (uses a separate dummy tree for the tree-view UI)
        let (memtree_console, memtree_handler) = {
            let dummy_tree = Arc::new(RwLock::new(crate::memory::MemTree::new()));
            let console = crate::cli::memtree_console::MemTreeConsole::new(dummy_tree);
            let handler = crate::cli::memtree_console::EventHandler::new();
            (
                Arc::new(RwLock::new(console)),
                Arc::new(tokio::sync::Mutex::new(handler)),
            )
        };

        let (peer_tx, peer_session_rx) =
            tokio::sync::broadcast::channel::<crate::session::SessionEvent>(128);

        let participant_subject = local_participant_subject();
        let runner_subject = runner_subject_from(&participant_subject, Uuid::new_v4());

        Self {
            event_rx,
            event_tx,
            input_rx,
            conversation,
            query_states: Arc::new(QueryStateManager::new()),
            model_selection: ModelSelection::from_handle(
                active_provider_index,
                provider_resolver.generator_handle(),
            ),
            qwen_gen,
            available_providers,
            daemon_client,
            router,
            generator_state,
            tool_definitions: Arc::new(RwLock::new(tool_definitions)),
            tui_renderer,
            output_manager,
            status_bar,
            streaming_enabled,
            tool_coordinator,
            program_runtime,
            tool_results: Arc::new(RwLock::new(std::collections::HashMap::new())),
            active_query_id: Arc::new(RwLock::new(None)),
            pending_queries: std::collections::VecDeque::new(),
            pending_named_brain_turns: std::collections::HashMap::new(),
            pending_named_brain_programs: std::collections::HashMap::new(),
            local_brain_projections: std::collections::VecDeque::new(),
            remote_brain_tool_unit: None,
            remote_brain_tool_rows: std::collections::HashMap::new(),
            remote_brain_approval_rows: std::collections::HashMap::new(),
            queued_remote_brain_approvals: std::collections::VecDeque::new(),
            active_remote_brain_approval: None,
            pending_approvals: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_vm_approval: None,
            ipc_client,
            mode,
            plan_content,
            memtree_console,
            memtree_handler,
            view_mode: Arc::new(RwLock::new(ViewMode::List)), // Start in list view
            active_tool_uses: Arc::new(RwLock::new(std::collections::HashMap::new())),
            feedback_logger: FeedbackLogger::new().ok(),
            metrics_logger: dirs::home_dir()
                .map(|h| h.join(".finch").join("metrics"))
                .and_then(|p| crate::metrics::MetricsLogger::new(p).ok())
                .map(Arc::new),
            memory_system,
            session_label,
            participant_subject,
            runner_subject,
            session_uuid: Uuid::new_v4(),
            cwd: String::new(), // populated at the start of run()
            context_lines,
            max_verbatim_messages,
            context_recall_k,
            todo_list,
            enable_summarization,
            auto_compact_enabled,
            pending_dialog_tx: None,
            pending_poset_run: None,
            tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
            current_graph: Arc::new(tokio::sync::Mutex::new(crate::graph::ExecutionGraph::new())),
            stack,
            poset,
            plan_word: None,
            peer_tx,
            diff_store: DiffStore::new(),
            peer_session_rx,
            active_remote_brain: None,
            home_brain: None,
            home_runner_lease_active: false,
            home_runner_lease_id: None,
            runner_brain: None,
            runner_renewal_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_home_runner_error: None,
            daemon_base_url,
            llm_tx,
            llm_rx: Some(llm_rx),
        }
    }

    /// Run the event loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::debug!("Event loop starting");
        // Signal that the TUI owns the terminal so proposal editors perform a
        // complete terminal-protocol handoff before launching $VISUAL/$EDITOR.
        crate::set_tui_active(true);

        // ── Startup header (Claude Code style) ───────────────────────────────
        // Clear accumulated startup noise from the output manager, then print a
        // clean header: finch version · primary model · working directory.
        self.output_manager.clear();

        let model_name = self.model_selection.generator().await.name().to_string();
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| {
                // Shorten $HOME prefix to ~
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rel) = p.strip_prefix(&home) {
                        return format!("~/{}", rel.display());
                    }
                }
                p.display().to_string()
            })
            .unwrap_or_else(|| "~".to_string());
        self.cwd = cwd.clone();
        let home_runner_state = match self.register_home_brain().await {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!("could not register home Brain: {error}");
                None
            }
        };
        self.home_runner_lease_id = home_runner_state
            .as_ref()
            .and_then(|state| state.as_ref().ok())
            .copied();
        self.home_runner_lease_active = self.home_runner_lease_id.is_some();
        self.runner_brain = self
            .home_runner_lease_active
            .then(|| self.session_label.clone());
        let home_runner_error = home_runner_state
            .as_ref()
            .and_then(|state| state.as_ref().err())
            .cloned();
        self.last_home_runner_error = home_runner_error.clone();
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            match &home_runner_state {
                Some(Ok(_)) => format!("◆ brain: {} · runner", self.session_label),
                Some(Err(_)) => {
                    format!("◆ brain: {} · home · no runner lease", self.session_label)
                }
                None => format!("◆ brain: {} · home · daemon offline", self.session_label),
            },
        );

        {
            let mut tui = self.tui_renderer.lock().await;
            tui.set_session_label(self.session_label.clone());
        }
        self.output_manager.write_info(TuiRenderer::startup_header(
            &model_name,
            &cwd,
            &self.session_label,
        ));
        if let Some(error) = home_runner_error {
            self.output_manager.write_info(format!(
                "{}: runner unavailable: {}",
                self.session_label, error
            ));
        }
        if self.daemon_base_url.is_some() {
            if let Err(error) = self.attach_home_brain().await {
                self.output_manager.write_info(format!(
                    "{}: home Brain event stream unavailable: {}",
                    self.session_label, error
                ));
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        // Show weekly license notice for non-commercial users (honor system)
        {
            use crate::config::{load_config, LicenseType};
            use chrono::NaiveDate;
            if let Ok(mut cfg) = load_config() {
                if cfg.license.license_type == LicenseType::Noncommercial {
                    let today = chrono::Local::now().date_naive();
                    let suppress_until = cfg
                        .license
                        .notice_suppress_until
                        .as_deref()
                        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                    let should_show = suppress_until.is_none_or(|d| today > d);
                    if should_show {
                        // Startup notices are application status, not a
                        // conversation artifact. Keeping this out of the
                        // output manager prevents it from becoming stale
                        // scrollback or racing the first shadow-buffer frame.
                        self.status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::Custom(
                                "license-notice".to_string(),
                            ),
                            "Using Finch commercially? $10/yr · finch license activate --key <key>",
                        );
                        let new_date = (today + chrono::Duration::days(7))
                            .format("%Y-%m-%d")
                            .to_string();
                        cfg.license.notice_suppress_until = Some(new_date);
                        let _ = cfg.save(); // non-fatal if save fails
                    }
                }
            }
        }

        // Apply auto-compact setting to the conversation history
        if !self.auto_compact_enabled {
            self.conversation.write().await.set_auto_compact(false);
        }

        // Initialize compaction status display (suppressed when auto-compact disabled)
        if self.auto_compact_enabled {
            self.update_compaction_status().await;
        }

        // Initialize plan mode indicator (starts in Normal mode)
        self.update_plan_mode_indicator(&crate::cli::repl::ReplMode::Normal);

        // Set initial memory context in status bar
        if let Some(ref mem) = self.memory_system {
            if mem.stats().await.is_ok() {
                let engine = if NeuralEmbeddingEngine::find_in_cache().is_some() {
                    "neural"
                } else {
                    "tfidf"
                };
                self.status_bar.update_line(
                    crate::cli::status_bar::StatusLineType::MemoryContext,
                    format!("🧠 {engine}  ·  recalled 0"),
                );
            }
        }

        // Set initial terminal window/tab title (no topic yet on fresh start)
        {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::terminal::SetTitle(format!("finch · {} · {}", self.session_label, cwd))
            );
        }

        // Attempt initial summary — populates on restart from previous memory
        if let Some(ref mem) = self.memory_system {
            refresh_context_strip(
                mem,
                &self.session_label,
                &cwd,
                &self.status_bar,
                self.context_lines,
            )
            .await;
        }

        // ── Spawn LLM worker loop ─────────────────────────────────────────────
        // Hand the receiver half of the channel to LlmLoop so it runs as its own
        // Tokio task, decoupled from TUI select! timing.
        {
            let llm_rx = self.llm_rx.take().expect("LlmLoop already started");
            let llm_loop = LlmLoop::new(
                llm_rx,
                self.event_tx.clone(),
                self.model_selection.generator_handle(),
                Arc::clone(&self.qwen_gen),
                Arc::clone(&self.router),
                Arc::clone(&self.generator_state),
                Arc::clone(&self.tool_definitions),
                self.tool_coordinator.clone(),
                Arc::clone(&self.program_runtime),
                Arc::clone(&self.tool_call_history),
                Arc::clone(&self.conversation),
                Arc::clone(&self.query_states),
                Arc::clone(&self.mode),
                Arc::clone(&self.output_manager),
                Arc::clone(&self.status_bar),
                Arc::clone(&self.tui_renderer),
                Arc::clone(&self.active_tool_uses),
                self.memory_system.clone(),
                Arc::clone(&self.current_graph),
                self.session_label.clone(),
                self.cwd.clone(),
                self.context_lines,
                self.max_verbatim_messages,
                self.context_recall_k,
                self.enable_summarization,
                self.auto_compact_enabled,
                self.metrics_logger.clone(),
            );
            tokio::spawn(llm_loop.run());
        }

        // Render interval (33ms ≈ 30fps) — smooth streaming without terminal flicker.
        // 60fps (16ms) caused visual artifacts on most terminal emulators; 30fps is the
        // sweet spot: fast enough to feel live, slow enough to not tear.
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));

        // Cleanup interval (30 seconds)
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        // Flag to control the loop
        let mut should_exit = false;

        while !should_exit {
            tokio::select! {
                // User input event
                Some(event) = self.input_rx.recv() => {
                    use crate::cli::tui::InputEvent;
                    match event {
                        InputEvent::Submitted(input) => {
                            tracing::debug!("Received input: {}", input);
                            // Clear typing words — restore panel to previous mode.
                            {
                                let mut tui = self.tui_renderer.lock().await;
                                tui.set_typing_words(vec![]);
                            }
                            self.handle_user_input(input).await?;
                        }
                        InputEvent::TypingStarted(partial) => {
                            tracing::debug!("Typing started: {} chars", partial.len());
                            self.handle_typing_started(partial).await;
                        }
                    }
                }

                // REPL event (query complete, tool result, etc.)
                Some(event) = self.event_rx.recv() => {
                    let event_name = match &event {
                        ReplEvent::StreamingComplete { .. } => "StreamingComplete",
                        ReplEvent::QueryComplete { .. } => "QueryComplete",
                        ReplEvent::QueryFailed { .. } => "QueryFailed",
                        ReplEvent::ToolResult { .. } => "ToolResult",
                        ReplEvent::ToolCallsStarted { .. } => "ToolCallsStarted",
                        ReplEvent::ToolApprovalNeeded { .. } => "ToolApprovalNeeded",
                        ReplEvent::VmApprovalNeeded { .. } => "VmApprovalNeeded",
                        ReplEvent::OutputReady { .. } => "OutputReady",
                        ReplEvent::VmEffect { .. } => "VmEffect",
                        ReplEvent::VmOutputComplete { .. } => "VmOutputComplete",
                        ReplEvent::TypedProgramComplete { .. } => "TypedProgramComplete",
                        ReplEvent::UserInput { .. } => "UserInput",
                        ReplEvent::StatsUpdate { .. } => "StatsUpdate",
                        ReplEvent::AgentLifecycle(_) => "AgentLifecycle",
                        ReplEvent::CancelQuery => "CancelQuery",
                        ReplEvent::Shutdown => "Shutdown",
                        ReplEvent::ShowDialog { .. } => "ShowDialog",
                        ReplEvent::PosetComplete { result: Ok(_) } => "PosetComplete(ok)",
                        ReplEvent::PosetComplete { result: Err(_) } => "PosetComplete(err)",
                        ReplEvent::LispResult { result: Ok(_) } => "LispResult(ok)",
                        ReplEvent::LispResult { result: Err(_) } => "LispResult(err)",
                        ReplEvent::RemoteBrainMessage { .. } => "RemoteBrainMessage",
                        ReplEvent::RemoteBrainError { .. } => "RemoteBrainError",
                        ReplEvent::RemoteBrainDisconnected { .. } => "RemoteBrainDisconnected",
                        ReplEvent::RunnerLeaseStatus { .. } => "RunnerLeaseStatus",
                        ReplEvent::NamedBrainProgramRequested(_) => "NamedBrainProgramRequested",
                        ReplEvent::NamedBrainTurnRequested(_) => "NamedBrainTurnRequested",
                        ReplEvent::NamedBrainRunCancelRequested(_) => {
                            "NamedBrainRunCancelRequested"
                        }
                        ReplEvent::NamedBrainProgramFinished(_) => "NamedBrainProgramFinished",
                    };
                    tracing::debug!("[EVENT_LOOP] Received event: {}", event_name);
                    tracing::debug!("Received event: {:?}", event);
                    if matches!(event, ReplEvent::Shutdown) {
                        should_exit = true;
                    } else {
                        tracing::debug!("[EVENT_LOOP] Handling {}...", event_name);
                        self.handle_event(event).await?;
                        tracing::debug!("[EVENT_LOOP] {} handled", event_name);
                    }
                }

                // Periodic rendering
                _ = render_interval.tick() => {
                    // Only rotate the poset 3D view if the Co-Forth panel has content to display.
                    // This saves significant CPU when the panel is empty (most of the time).
                    {
                        let tui = self.tui_renderer.lock().await;
                        if let Some(text) = tui.corner.lock().ok().and_then(|g| g.clone()) {
                            if !text.trim().is_empty() {
                                drop(tui); // Release TUI lock before poset lock
                                // Slowly rotate the poset 3D view at 33ms/tick (0.00265 rad ≈ same ~12s turn)
                                self.poset.lock().await.rotate(0.00265, 0.0);
                            }
                        }
                    }

                    // Single mutex acquisition: read all pending TUI state in one lock.
                    // Reduces contention with spawn_input_task from 3-4 round-trips to 1 per tick.
                    let (pending_cancel, dialog_result, pending_feedback) = {
                        let mut tui = self.tui_renderer.lock().await;
                        (
                            std::mem::take(&mut tui.pending_cancellation),
                            tui.pending_dialog_result.take(),
                            tui.pending_feedback.take(),
                        )
                    };

                    if pending_cancel {
                        let _ = self.event_tx.send(ReplEvent::CancelQuery);
                    }

                    // Route pending dialog result (tool approval, brain question, ShowDialog oneshot, etc.)
                    if let Some(dialog_result) = dialog_result {
                        {
                            // Priority 0: ShowDialog (used by PresentPlan, AskUserQuestion, etc.)
                            if let Some(tx) = self.pending_dialog_tx.take() {
                                let _ = tx.send(dialog_result);
                            }
                            // Priority 1: Poset run confirmation (state machine — no oneshot)
                            else if let Some(pending) = self.pending_poset_run.take() {
                                if matches!(dialog_result, crate::cli::tui::DialogResult::Selected(0)) {
                                    let PendingPosetRun { generator, poset, event_tx } = pending;
                                    tokio::spawn(async move {
                                        let result = crate::poset::executor::execute_poset(
                                            Arc::new(tokio::sync::Mutex::new(poset)),
                                            generator,
                                        ).await;
                                        let _ = event_tx.send(super::events::ReplEvent::PosetComplete { result });
                                    });
                                    self.output_manager.write_info("running");
                                    self.render_tui().await.ok();
                                }
                            }
                            // Priority 2: addressed named-Brain approval.
                            else if let Some(pending) =
                                self.active_remote_brain_approval.take()
                            {
                                let decision = match &pending.kind {
                                    RemoteBrainApprovalKind::Tool(tool_use) => {
                                        let is_file_mutating = matches!(
                                            tool_use.name.as_str(),
                                            "write" | "Write" | "edit" | "Edit"
                                        );
                                        let is_editor_option = is_file_mutating
                                            && matches!(
                                                dialog_result,
                                                crate::cli::tui::DialogResult::Selected(1)
                                            );
                                        let confirmation = if is_editor_option {
                                            let proposed = tool_use
                                                .input
                                                .get("content")
                                                .or_else(|| tool_use.input.get("new_string"))
                                                .and_then(serde_json::Value::as_str)
                                                .unwrap_or("");
                                            match open_in_editor(proposed) {
                                                Ok(edited) => {
                                                    let mut input = tool_use.input.clone();
                                                    if input.get("content").is_some() {
                                                        input["content"] =
                                                            serde_json::Value::String(edited);
                                                    } else {
                                                        input["new_string"] =
                                                            serde_json::Value::String(edited);
                                                    }
                                                    super::events::ConfirmationResult::ApproveWithInput(
                                                        input,
                                                    )
                                                }
                                                Err(error) => {
                                                    tracing::warn!(
                                                        "remote approval editor failed: {error}"
                                                    );
                                                    super::events::ConfirmationResult::Deny
                                                }
                                            }
                                        } else {
                                            let adjusted = if is_file_mutating {
                                                match dialog_result {
                                                    crate::cli::tui::DialogResult::Selected(0) => {
                                                        crate::cli::tui::DialogResult::Selected(0)
                                                    }
                                                    crate::cli::tui::DialogResult::Selected(index) => {
                                                        crate::cli::tui::DialogResult::Selected(
                                                            index - 1,
                                                        )
                                                    }
                                                    other => other,
                                                }
                                            } else {
                                                dialog_result
                                            };
                                            dialog_result_to_confirmation(adjusted, tool_use)
                                        };
                                        confirmation_audit_value(&confirmation)
                                    }
                                    RemoteBrainApprovalKind::Vm { choices, .. } => {
                                        let choice = match dialog_result {
                                            crate::cli::tui::DialogResult::Selected(index) => choices
                                                .get(index)
                                                .cloned()
                                                .unwrap_or(crate::vm::ApprovalChoice::Deny),
                                            _ => crate::vm::ApprovalChoice::Deny,
                                        };
                                        serde_json::to_value(choice).unwrap_or_else(|_| {
                                            serde_json::json!({"choice": "deny"})
                                        })
                                    }
                                };
                                let client = pending.client;
                                let target = client.target.display_name();
                                let event_tx = self.event_tx.clone();
                                tokio::task::spawn_local(async move {
                                    if let Err(error) = client
                                        .push(crate::brain::shared::BrainEventKind::ApprovalDecided {
                                            request_seq: pending.request_seq,
                                            approval_id: pending.approval_id,
                                            decision,
                                        })
                                        .await
                                    {
                                        let _ = event_tx.send(ReplEvent::RemoteBrainError {
                                            target,
                                            error: error.to_string(),
                                        });
                                    }
                                });
                            } else if let Some(pending) = self.pending_vm_approval.take() {
                                let choice = match dialog_result {
                                    crate::cli::tui::DialogResult::Selected(index) => pending
                                        .choices
                                        .get(index)
                                        .cloned()
                                        .unwrap_or(crate::vm::ApprovalChoice::Deny),
                                    _ => crate::vm::ApprovalChoice::Deny,
                                };
                                if let Some(query_id) = pending.query_id {
                                    if let Some(turn) =
                                        self.pending_named_brain_turns.get_mut(&query_id)
                                    {
                                        turn.turn_events.push(
                                            crate::server::RunnerTurnEvent::ApprovalDecided {
                                                approval_id: pending.approval_id,
                                                decision: serde_json::to_value(&choice)
                                                    .unwrap_or_else(|_| serde_json::json!({
                                                        "choice": "serialization_error"
                                                    })),
                                            },
                                        );
                                    }
                                }
                                let _ = pending.response_tx.send(choice);
                            } else {
                                // Find which query this dialog was for (tool approval)
                                let mut approvals = self.pending_approvals.write().await;

                                if approvals.is_empty() {
                                    // No handler consumed the result — ShowDialog result arrived
                                    // before pending_dialog_tx was set (belt-and-suspenders race).
                                    // Put it back so the next tick delivers it once the tx is ready.
                                    drop(approvals);
                                    let mut tui = self.tui_renderer.lock().await;
                                    tui.pending_dialog_result = Some(dialog_result);
                                } else if let Some((query_id, (_tool_use, _response_tx))) = approvals.iter().next() {
                                    let query_id = *query_id;
                                    let (tool_use, response_tx) = approvals.remove(&query_id)
                                        .expect("query_id was just obtained from the same map");

                                    // Check for "Edit in $EDITOR" (option index 1 for write/edit tools)
                                    let is_file_mutating = matches!(tool_use.name.as_str(), "write" | "Write" | "edit" | "Edit");
                                    let is_editor_option = is_file_mutating && matches!(dialog_result, crate::cli::tui::DialogResult::Selected(1));

                                    let confirmation = if is_editor_option {
                                        // Extract proposed content
                                        let proposed = tool_use.input.get("content")
                                            .or_else(|| tool_use.input.get("new_string"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();

                                        // Write to temp file and open editor
                                        match open_in_editor(&proposed) {
                                            Ok(edited) => {
                                                let mut new_input = tool_use.input.clone();
                                                if tool_use.input.get("content").is_some() {
                                                    new_input["content"] = serde_json::Value::String(edited);
                                                } else {
                                                    new_input["new_string"] = serde_json::Value::String(edited);
                                                }
                                                super::events::ConfirmationResult::ApproveWithInput(new_input)
                                            }
                                            Err(e) => {
                                                tracing::warn!("Editor failed: {}", e);
                                                super::events::ConfirmationResult::Deny
                                            }
                                        }
                                    } else {
                                        // Shift option indices for file-mutating tools (extra "Edit" option at 1)
                                        let adjusted_result = if is_file_mutating {
                                            match dialog_result {
                                                crate::cli::tui::DialogResult::Selected(0) => dialog_result,
                                                crate::cli::tui::DialogResult::Selected(n) => crate::cli::tui::DialogResult::Selected(n - 1),
                                                other => other,
                                            }
                                        } else {
                                            dialog_result
                                        };
                                        self.dialog_result_to_confirmation(adjusted_result, &tool_use)
                                    };

                                    if let Some(turn) =
                                        self.pending_named_brain_turns.get_mut(&query_id)
                                    {
                                        turn.turn_events.push(
                                            crate::server::RunnerTurnEvent::ApprovalDecided {
                                                approval_id: tool_use.id.clone(),
                                                decision: confirmation_audit_value(&confirmation),
                                            },
                                        );
                                    }

                                    // Send confirmation back to tool execution task
                                    let _ = response_tx.send(confirmation);

                                    tracing::debug!("[EVENT_LOOP] Tool approval processed for query {}", query_id);
                                }
                            }
                        }
                        self.try_present_remote_brain_approval().await?;
                    }

                    if let Some(rating) = pending_feedback {
                        let (weight, label) = match rating {
                            FeedbackRating::Good => (1.0_f64, "👍 Good"),
                            FeedbackRating::Bad  => (10.0_f64, "👎 Bad"),
                        };
                        self.handle_feedback_command(weight, rating, None).await?;
                        tracing::debug!("[EVENT_LOOP] Quick feedback recorded: {}", label);
                    }

                    // Don't spam logs, but good to know the loop is alive
                    // tracing::debug!("[EVENT_LOOP] Render tick");
                    if let Err(e) = self.render_tui().await {
                        tracing::warn!("TUI render failed in event loop: {}", e);
                        // Set recovery flag for next tick
                        let mut tui = self.tui_renderer.lock().await;
                        tui.needs_full_refresh = true;
                        tui.last_render_error = Some(e.to_string());
                        // Continue event loop - don't crash
                    }
                }

                // Periodic cleanup
                _ = cleanup_interval.tick() => {
                    self.cleanup_old_queries().await;
                }

                // Structured session events from peers (Diff proposals, edits, accepts, rejects)
                ev = self.peer_session_rx.recv() => {
                    match ev {
                        Ok(crate::session::SessionEvent::Diff { id, label, patch, description }) => {
                            // Find the proposer name from session list (unknown if not found)
                            let proposed_by = "peer".to_string();
                            self.diff_store.propose(id, label.clone(), patch.clone(), description.clone(), proposed_by.clone());
                            self.render_diff_proposal(id, &label, &patch, description.as_deref(), &proposed_by);
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after Diff proposal failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffEdit { diff_id, patch, description }) => {
                            self.diff_store.edit(diff_id, patch.clone(), description.clone());
                            if let Some(d) = self.diff_store.get(diff_id) {
                                let label = d.label.clone();
                                let proposed_by = d.proposed_by.clone();
                                use crossterm::style::Stylize;
                                self.output_manager.write_info(format!(
                                    "{}  {} revised diff for {}",
                                    "↻".yellow(),
                                    proposed_by.as_str().cyan(),
                                    label.as_str().white(),
                                ));
                                self.render_diff_proposal(diff_id, &label, &patch, description.as_deref(), &proposed_by);
                            }
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffEdit failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffAccept { diff_id }) => {
                            if let Some(d) = self.diff_store.accept(diff_id) {
                                use crossterm::style::Stylize;
                                self.output_manager.write_info(format!(
                                    "{}  diff {} accepted",
                                    "✓".green(),
                                    &d.id.to_string()[..8].white(),
                                ));
                            }
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffAccept failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffReject { diff_id, reason }) => {
                            self.diff_store.reject(diff_id, reason.clone());
                            use crossterm::style::Stylize;
                            let reason_str = reason.as_deref().unwrap_or("no reason given");
                            self.output_manager.write_info(format!(
                                "{}  diff {} rejected: {}",
                                "✗".red(),
                                &diff_id.to_string()[..8].white(),
                                reason_str.dark_grey(),
                            ));
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffReject failed: {e}");
                            }
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Other session events or lag — ignore here (handled elsewhere)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Channel closed — nothing to do
                        }
                    }
                }
            }
        }

        // Release durable Brain presence before the TUI shuts down. `/quit`
        // reaches this path rather than bypassing cleanup with process::exit.
        self.release_home_brain_presence().await;

        // Save persistent state before the TUI shuts down and the terminal goes read-only.
        {
            let mut executor = self.tool_coordinator.tool_executor().lock().await;
            let _ = executor.save_if_dirty();
        }

        // Normal exit — shut down TUI and restore terminal before returning.
        {
            let mut tui = self.tui_renderer.lock().await;
            let _ = tui.shutdown();
        }

        // Save conversation to ~/.finch/sessions/<uuid>.json and print the UUID.
        // The user can resume with: finch --resume <uuid>
        if let Some(home) = dirs::home_dir() {
            let sessions_dir = home.join(".finch").join("sessions");
            if std::fs::create_dir_all(&sessions_dir).is_ok() {
                let id = self.session_uuid;
                let path = sessions_dir.join(format!("{id}.json"));
                let history = self.conversation.read().await.clone();
                if !history.is_empty() {
                    if history.save(&path).is_ok() {
                        println!("\n{id}");
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle user input (query or command)
    async fn handle_user_input(&mut self, input: String) -> Result<()> {
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
                        {
                            let mut tui = self.tui_renderer.lock().await;
                            tui.resume().ok();
                        }
                        match wizard_result {
                            Ok(Ok(Some(result))) => {
                                // Save the new config.
                                if let Err(e) = crate::cli::setup_wizard::apply_and_save(&result) {
                                    self.output_manager
                                        .write_info(format!("Setup saved with error: {e}"));
                                } else {
                                    self.output_manager.write_info(
                                        "Settings saved. Restart finch to apply changes."
                                            .to_string(),
                                    );
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
                            self.push_remote_brain(crate::brain::shared::BrainEventKind::Program {
                                language: crate::brain::shared::ProgramLanguage::Forth,
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
                    Command::BrainRuns => {
                        self.handle_brain_runs().await?;
                    }
                    Command::BrainRunCancel(prefix) => {
                        if let Err(error) = self.handle_brain_run_cancel(prefix).await {
                            self.output_manager
                                .write_info(format!("brain cancel: {error}"));
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

        // Forth word definition: `: word ... ;`
        // Route directly to the Forth VM — do not push as a vocabulary word.
        if input.trim().starts_with(": ") {
            if self.selected_brain().is_some() {
                return self
                    .push_remote_brain(crate::brain::shared::BrainEventKind::Program {
                        language: crate::brain::shared::ProgramLanguage::Forth,
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
                    .push_remote_brain(crate::brain::shared::BrainEventKind::Program {
                        language: crate::brain::shared::ProgramLanguage::Lisp,
                        source: input,
                    })
                    .await;
            }
            return self
                .execute_interactive_typed_program(
                    crate::programs::ProgramLanguage::Lisp,
                    input,
                )
                .await;
        }

        // Plain terminal text is always a user turn. Executable source is
        // deliberately explicit: Lisp begins with `(`, typed definitions begin
        // with `:`, and other Co-Forth uses `/forth`. Never classify prose by
        // asking the historical semiotic dictionary whether its words exist.
        self.execute_query(input).await
    }

    /// Execute explicit interactive source through the same typed runtime and
    /// portable output projection as provider wire responses.  Legacy Lisp is
    /// intentionally not a fallback here: an opening `(` is an unambiguous
    /// Finch-Lisp program and must receive typed diagnostics.
    async fn execute_interactive_typed_program(
        &mut self,
        language: crate::programs::ProgramLanguage,
        source: String,
    ) -> Result<()> {
        let source_unit = self.output_manager.start_work_unit("typed program");
        source_unit.set_program_source(language.as_str());
        source_unit.set_response(source.clone());
        source_unit.set_complete();
        let output_unit = self.output_manager.start_work_unit("VM program output");
        output_unit.set_program_output();
        let projection =
            VmOutputProjection::new(Arc::clone(&self.output_manager), Arc::clone(&output_unit));
        let event_tx = self.event_tx.clone();
        let sink: crate::runtime::TypedEffectSink = Arc::new(move |envelope| {
            let _ = event_tx.send(ReplEvent::VmEffect {
                projection: projection.clone(),
                envelope,
            });
        });
        let submission = crate::runtime::ProgramSubmission {
            language,
            source_id: Some(format!("interactive.{}", language.as_str())),
            source,
            intent: "interactive typed source".into(),
            effect: crate::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: self.program_runtime.manifest_generation(),
            expected_revision: Some(self.program_runtime.revision()),
            budget: None,
        };
        let runtime = Arc::clone(&self.program_runtime);
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let result = async {
                let outcome = runtime
                    .submit_with_deferred_program_effects(submission, sink)
                    .await?;
                super::query_processor::resume_interactive_boundaries(
                    &runtime,
                    event_tx.clone(),
                    outcome,
                )
                .await
            }
            .await
            .map_err(|error: anyhow::Error| error.to_string());
            let _ = event_tx.send(ReplEvent::TypedProgramComplete {
                output_unit,
                result,
            });
        });
        self.render_tui().await
    }

    /// Execute a query with echo (used by /run where the query hasn't been displayed yet).
    async fn execute_query(&mut self, input: String) -> Result<()> {
        self.execute_query_inner(input, true, false).await
    }

    /// Execute a conversational response to a word push — no tools, no brain context injection.
    async fn execute_chat_response(&mut self, input: String) -> Result<()> {
        self.execute_query_inner(input, false, true).await
    }

    /// Execute a query directly — called by /run after draining the stack, or
    /// after a user push (where the echo was already written).
    /// `echo` — whether to write the user query to the output buffer.
    /// `chat_only` — suppress tools and brain context (for word-push conversational responses).
    async fn execute_query_inner(
        &mut self,
        input: String,
        echo: bool,
        chat_only: bool,
    ) -> Result<()> {
        if self.selected_brain().is_some() {
            return self
                .push_remote_brain(crate::brain::shared::BrainEventKind::Prompt { text: input })
                .await;
        }

        // One interactive Brain owns a single ordered conversation and VM
        // revision. Queue later user turns rather than racing both through the
        // same mutable state. Preserve the input's normal echo semantics now,
        // then start it without a second echo after the active turn commits.
        if self.active_query_id.read().await.is_some() {
            if echo {
                self.output_manager.write_user(input.clone());
            }
            self.pending_queries.push_back((input, false, chat_only));
            return Ok(());
        }

        // Drain any pending images from TUI (pasted before sending)
        let pending_images: Vec<(String, String)> = {
            let mut tui = self.tui_renderer.lock().await;
            tui.pending_images
                .drain(..)
                .map(|(_idx, b64, media_type)| (media_type, b64))
                .collect()
        };

        // Echo query to output buffer (skip when caller already echoed)
        if echo {
            self.output_manager.write_user(input.clone());
        }

        // Create a new query
        let conversation_snapshot = self.conversation.read().await.snapshot();
        let query_id = self.query_states.create_query(conversation_snapshot).await;

        // Add user message to conversation (with images if any were pasted)
        if pending_images.is_empty() {
            self.conversation
                .write()
                .await
                .add_user_message(input.clone());
        } else {
            self.conversation
                .write()
                .await
                .add_user_message_with_images(input.clone(), &pending_images);
        }

        // Update compaction percentage in status bar
        self.update_compaction_status().await;

        // Set as active query (for cancellation)
        *self.active_query_id.write().await = Some(query_id);

        // Send query to the LLM worker loop (no tools for chat_only word-push responses)
        let _ = self.llm_tx.send(LlmRequest::Query {
            id: query_id,
            text: input,
            no_tools: chat_only,
        });

        Ok(())
    }

    /// Handle feedback commands (/critical, /medium, /good) and Ctrl+G/Ctrl+B quick ratings.
    ///
    /// Finds the last user query and assistant response from conversation history,
    /// logs a `FeedbackEntry` to `~/.finch/feedback.jsonl`, and prints a confirmation.
    async fn handle_feedback_command(
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

    /// Handle /local command - query local model directly (bypass routing)
    async fn handle_local_query(&mut self, query: String) -> Result<()> {
        use crate::cli::messages::StreamingResponseMessage;

        let Some(ref ipc) = self.ipc_client else {
            self.output_manager
                .write_error("Error: /local requires the daemon.");
            self.output_manager
                .write_info("    Start the daemon: finch daemon --bind 127.0.0.1:11435");
            return self.render_tui().await;
        };

        let msg = Arc::new(StreamingResponseMessage::new());
        msg.append_chunk("🔧 Local Model Query (bypassing routing)\n\n");
        self.output_manager
            .add_trait_message(msg.clone() as Arc<dyn crate::cli::messages::Message>);
        self.render_tui().await?;

        let messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![crate::claude::ContentBlock::Text { text: query }],
        }];

        let mut rx = match ipc.query_stream(messages, vec![]).await {
            Ok(rx) => rx,
            Err(e) => {
                msg.set_failed();
                self.output_manager
                    .write_error(format!("Local query failed: {}", e));
                return self.render_tui().await;
            }
        };

        // Drive the stream in a local task so the event loop keeps rendering
        let msg_clone = msg.clone();
        let output_mgr = self.output_manager.clone();
        tokio::task::spawn_local(async move {
            use crate::generators::StreamChunk;
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(StreamChunk::TextDelta(t)) => msg_clone.append_chunk(&t),
                    Ok(_) => {} // Usage, ContentBlockComplete — ignored
                    Err(e) => {
                        msg_clone.set_failed();
                        output_mgr.write_error(format!("Local query error: {}", e));
                        return;
                    }
                }
            }
            // Channel closed = stream complete
            msg_clone.append_chunk("\n✓ Local model (bypassed routing)");
            msg_clone.set_complete();
        });

        Ok(())
    }

    async fn handle_provider_show(&self) {
        let active = self.model_selection.active_index().await;
        let Some(entry) = self.available_providers.get(active) else {
            self.output_manager.write_info("No active model profile.");
            return;
        };

        let mut text = format!(
            "Active model: {}\n  provider: {}\n  model: {}\n  conversation: preserved across switches",
            entry.profile_name(),
            entry.provider_type(),
            entry.model().unwrap_or("provider default")
        );
        if let Some(pending) = self.model_selection.pending_index().await {
            if let Some(entry) = self.available_providers.get(pending) {
                text.push_str(&format!(
                    "\n  pending: {} (waiting for local model startup)",
                    entry.profile_name()
                ));
            }
        }
        self.output_manager.write_info(text);
    }

    /// Handle `/model <name>` — switch the active named provider profile.
    async fn handle_provider_switch(&mut self, name: String) -> Result<()> {
        use crate::generators::claude::ClaudeGenerator;
        use crate::providers::create_provider_from_entry;

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
            match create_provider_from_entry(&entry) {
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to create model '{}': {}", name, e));
                }
                Ok(provider) => {
                    let client = crate::claude::ClaudeClient::with_provider(provider);
                    let inner: Arc<dyn Generator> =
                        Arc::new(ClaudeGenerator::new(Arc::new(client)));
                    let new_gen: Arc<dyn Generator> = Arc::new(
                        crate::generators::ProfiledGenerator::new(entry.profile_name(), inner),
                    );
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

    /// Handle /mcp list command - list connected MCP servers
    async fn handle_mcp_list(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            let servers = mcp_client.list_servers().await;
            if servers.is_empty() {
                self.output_manager.write_info("No MCP servers connected.");
            } else {
                let mut output = String::from("📡 Connected MCP Servers:\n\n");
                for server_name in servers {
                    output.push_str(&format!("  • {}\n", server_name));
                }
                self.output_manager.write_info(output);
            }
        } else {
            self.output_manager.write_info(
                "MCP plugin system not configured.\n\
                 Add MCP servers to ~/.finch/config.toml to get started.",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp tools command - list tools from servers
    async fn handle_mcp_tools(&mut self, server_filter: Option<String>) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            let all_tools = mcp_client.list_tools().await;
            let filtered_tools: Vec<_> = all_tools
                .into_iter()
                .filter(|tool| {
                    if let Some(ref server) = server_filter {
                        // Tool names are prefixed with "mcp_<server>_"
                        tool.name.starts_with(&format!("mcp_{}_", server))
                    } else {
                        true
                    }
                })
                .collect();

            if filtered_tools.is_empty() {
                if let Some(server) = server_filter {
                    self.output_manager.write_info(format!(
                        "No tools found for server '{}'. Check server name with /mcp list",
                        server
                    ));
                } else {
                    self.output_manager.write_info("No MCP tools available.");
                }
            } else {
                let header = if let Some(server) = server_filter {
                    format!("🔧 MCP Tools from '{}' server:\n\n", server)
                } else {
                    String::from("🔧 All MCP Tools:\n\n")
                };

                let mut output = header;
                for tool in filtered_tools {
                    // Remove "mcp_" prefix for display
                    let display_name = tool.name.strip_prefix("mcp_").unwrap_or(&tool.name);
                    output.push_str(&format!("  • {}\n", display_name));
                    output.push_str(&format!("    {}\n", tool.description));
                }
                self.output_manager.write_info(output);
            }
        } else {
            self.output_manager.write_info(
                "MCP plugin system not configured.\n\
                 Add MCP servers to ~/.finch/config.toml to get started.",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp refresh command - refresh tools from all servers
    async fn handle_mcp_refresh(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            let mcp_client = Arc::clone(mcp_client);
            self.output_manager.write_info("Refreshing MCP tools...");
            self.render_tui().await?;

            match mcp_client.refresh_all_tools().await {
                Ok(()) => {
                    let tools = mcp_client.list_tools().await;
                    *self.tool_definitions.write().await = executor_guard.list_all_tools().await;
                    drop(executor_guard);
                    match self.program_runtime.bind_mcp_client(mcp_client).await {
                        Ok(rejected) => {
                            for diagnostic in &rejected {
                                tracing::warn!(
                                    "MCP tool was not published to typed VM after refresh: {diagnostic}"
                                );
                            }
                            self.output_manager.write_info(format!(
                                "✓ Refreshed MCP tools ({} tools available, {} typed binding{} rejected)",
                                tools.len(),
                                rejected.len(),
                                if rejected.len() == 1 { "" } else { "s" }
                            ));
                        }
                        Err(error) => self.output_manager.write_error(format!(
                            "MCP tools refreshed, but typed VM vocabulary update failed: {error:#}"
                        )),
                    }
                }
                Err(e) => {
                    self.output_manager
                        .write_error(format!("Failed to refresh MCP tools: {}", e));
                }
            }
        } else {
            self.output_manager.write_info("No MCP servers configured.");
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp reload command - reconnect to all servers
    async fn handle_mcp_reload(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;
        if let Some(mcp_client) = executor_guard.mcp_client() {
            let mcp_client = Arc::clone(mcp_client);
            self.output_manager
                .write_info("Reconnecting to configured MCP servers...");
            mcp_client.reload().await?;
            let servers = mcp_client.list_servers().await;
            let tools = mcp_client.list_tools().await;
            *self.tool_definitions.write().await = executor_guard.list_all_tools().await;
            drop(executor_guard);
            match self.program_runtime.bind_mcp_client(mcp_client).await {
                Ok(rejected) => {
                    for diagnostic in &rejected {
                        tracing::warn!(
                            "MCP tool was not published to typed VM after reload: {diagnostic}"
                        );
                    }
                    self.output_manager.write_info(format!(
                        "✓ Connected to {} MCP server(s) with {} tool(s); {} typed binding{} rejected",
                        servers.len(),
                        tools.len(),
                        rejected.len(),
                        if rejected.len() == 1 { "" } else { "s" }
                    ));
                }
                Err(error) => self.output_manager.write_error(format!(
                    "MCP servers reconnected, but typed VM vocabulary update failed: {error:#}"
                )),
            }
        } else {
            self.output_manager.write_info("No MCP servers configured.");
        }
        self.render_tui().await?;
        Ok(())
    }

    /// Handle an event from the event channel
    async fn handle_event(&mut self, event: ReplEvent) -> Result<()> {
        match event {
            ReplEvent::UserInput { input } => {
                self.handle_user_input(input).await?;
            }

            ReplEvent::QueryComplete { query_id, response } => {
                // Add response to conversation
                self.conversation
                    .write()
                    .await
                    .add_assistant_message(response.clone());

                // Update compaction percentage in status bar
                self.update_compaction_status().await;

                // Update query state
                self.query_states
                    .update_state(
                        query_id,
                        QueryState::Completed {
                            response: response.clone(),
                        },
                    )
                    .await;

                // Display response
                self.output_manager.write_response(&response);
            }

            ReplEvent::QueryFailed { query_id, error } => {
                // DON'T remove streaming message here - fallback providers need it!
                // The message will be removed on StreamingComplete or stays for final error display

                // Update query state
                self.query_states
                    .update_state(
                        query_id,
                        QueryState::Failed {
                            error: error.clone(),
                        },
                    )
                    .await;

                if let Some(unit) = self.query_states.tool_work_unit(query_id).await {
                    unit.set_failed();
                    self.query_states.set_tool_work_unit(query_id, None).await;
                }

                // Display error
                self.output_manager
                    .write_error(format!("Query failed: {}", error));

                if let Some(pending) = self.pending_named_brain_turns.remove(&query_id) {
                    let _ = pending.response_tx.send(Err(crate::server::RunnerTurnError {
                        message: error.clone(),
                        turn_events: pending.turn_events,
                    }));
                }

                // Render TUI to ensure viewport is redrawn after error message
                if let Err(e) = self.render_tui().await {
                    tracing::warn!("Failed to render TUI after query error: {}", e);
                }

                // A terminal provider failure has no later StreamingComplete.
                // Release the turn so queued user input cannot wedge behind it.
                if *self.active_query_id.read().await == Some(query_id) {
                    *self.active_query_id.write().await = None;
                    if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                        self.execute_query_inner(next, echo, chat_only).await?;
                    }
                }
            }

            ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            } => {
                if let Some(proposal) = deferred_proposal_from_tool_result(&result) {
                    self.output_manager.write_status(format!(
                        "Proposal {} is awaiting editor review",
                        proposal.handle.sequence
                    ));
                    let approval_audience = self
                        .pending_named_brain_turns
                        .get(&query_id)
                        .map(|turn| turn.approval_audience.clone());
                    self.spawn_deferred_proposal(
                        query_id,
                        tool_id,
                        proposal,
                        approval_audience,
                    );
                } else if let Some(approval) = deferred_vm_approval_from_tool_result(&result) {
                    self.output_manager.write_status(format!(
                        "VM capability request {} is awaiting approval",
                        approval.prompt.request.id
                    ));
                    self.spawn_deferred_vm_approval(query_id, tool_id, approval);
                } else {
                    self.handle_tool_result(query_id, tool_id, result).await?;
                }
            }

            ReplEvent::ToolCallsStarted {
                query_id,
                tool_uses,
            } => {
                if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                    turn.turn_events.extend(tool_uses.into_iter().map(|tool_use| {
                        crate::server::RunnerTurnEvent::Call {
                            tool_id: tool_use.id,
                            name: tool_use.name,
                            input: tool_use.input,
                        }
                    }));
                }
            }

            ReplEvent::ToolApprovalNeeded {
                query_id,
                tool_use,
                response_tx,
            } => {
                self.handle_tool_approval_request(query_id, tool_use, response_tx)
                    .await?;
            }

            ReplEvent::VmApprovalNeeded {
                prompt,
                response_tx,
            } => {
                self.handle_vm_approval_request(prompt, response_tx).await?;
            }

            ReplEvent::OutputReady { message } => {
                self.output_manager.write_status(message);
            }

            ReplEvent::VmEffect {
                projection,
                envelope,
            } => {
                // A reconnecting application may deliver a journal suffix
                // more than once. Let the client-local projection reject
                // duplicates/gaps before rendering notices or mutating a
                // WorkUnit; the durable acknowledgement belongs to the later
                // Brain event-log layer.
                let projected = projection.project_envelope(envelope);
                if projected.is_empty() {
                    return Ok(());
                }
                for envelope in projected {
                    if envelope.effect.requirement.capability
                        != crate::vm::CapabilityKind::ProgramInvoke
                    {
                        continue;
                    }
                    let intent = match &envelope.effect.event {
                        crate::vm::HostSideEffect::Request { arguments } => arguments
                            .get(1)
                            .and_then(|value| match value {
                                crate::vm::TypedValue::String(text) => Some(text.as_str()),
                                _ => None,
                            })
                            .unwrap_or("Review proposed program"),
                        _ => "Review proposed program",
                    };
                    projection.append_default(&format!(
                        "Proposal awaiting review: {intent} [run {}, effect {}]",
                        envelope.execution_id, envelope.effect.sequence
                    ));
                }
                self.render_tui().await?;
            }

            ReplEvent::VmOutputComplete { output_unit } => {
                output_unit.set_complete();
                self.render_tui().await?;
            }

            ReplEvent::TypedProgramComplete {
                output_unit,
                result,
            } => {
                match result {
                    Ok(outcome)
                        if outcome.status == crate::runtime::outcome::ExecutionStatus::Completed => {}
                    Ok(outcome) => {
                        let detail = outcome.diagnostics.first().cloned().unwrap_or_else(|| {
                            format!("VM program ended as {:?}", outcome.status)
                        });
                        output_unit.append_response(&format!("VM error: {detail}"));
                    }
                    Err(error) => output_unit.append_response(&format!("VM error: {error}")),
                }
                output_unit.set_complete();
                self.render_tui().await?;
            }

            ReplEvent::StreamingComplete {
                query_id,
                full_response,
            } => {
                tracing::debug!("[EVENT_LOOP] Handling StreamingComplete event");

                // Check if this query is executing tools
                // If so, the assistant message was already added with ToolUse blocks
                let state = self
                    .query_states
                    .get_metadata(query_id)
                    .await
                    .map(|m| m.state.clone());
                let is_executing_tools = matches!(state, Some(QueryState::ExecutingTools { .. }));
                // The streaming path adds the assistant message and sets Completed before
                // sending StreamingComplete. The non-streaming path does not — it relies on
                // this handler to do both. Detect which case we are in.
                let already_completed = matches!(state, Some(QueryState::Completed { .. }));

                if !is_executing_tools && !already_completed {
                    tracing::debug!(
                        "[EVENT_LOOP] No tools, adding assistant message to conversation"
                    );
                    // Add complete response to conversation (only if not executing tools)
                    self.conversation
                        .write()
                        .await
                        .add_assistant_message(full_response.clone());
                    tracing::debug!("[EVENT_LOOP] Added assistant message to conversation");

                    // Update query state
                    self.query_states
                        .update_state(
                            query_id,
                            QueryState::Completed {
                                response: full_response.clone(),
                            },
                        )
                        .await;
                    tracing::debug!("[EVENT_LOOP] Updated query state");
                } else {
                    tracing::debug!("[EVENT_LOOP] Skipping duplicate message (tools={is_executing_tools}, already_completed={already_completed})");
                }

                // Update context usage indicator now that the message is committed
                self.update_compaction_status().await;

                self.finish_named_brain_turn(query_id, full_response.clone());

                // Render TUI to write the complete message to scrollback
                self.render_tui().await?;
                tracing::debug!("[EVENT_LOOP] StreamingComplete handled, TUI rendered");

                // Clear active query (query completed successfully)
                {
                    let mut active = self.active_query_id.write().await;
                    if *active == Some(query_id) {
                        *active = None;
                    }
                }
                if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                    self.execute_query_inner(next, echo, chat_only).await?;
                }
                // Record final response + save execution graph
                if !is_executing_tools {
                    let preview = full_response.chars().take(300).collect::<String>();
                    let mut g = self.current_graph.lock().await;
                    g.add_node(crate::graph::NodeKind::FinalResponse { preview });
                    if let Err(e) = g.save() {
                        tracing::warn!("Failed to save execution graph: {}", e);
                    }
                }

                // The AI does NOT auto-push to the stack on completion.
                // It pushes explicitly via the Push tool when it wants to
                // add something to the collaborative program.

                // Clear per-query tool-call history so it doesn't grow forever.
                self.tool_call_history.write().await.remove(&query_id);
            }

            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
            } => {
                // Record LLM invocation in execution graph
                self.current_graph
                    .lock()
                    .await
                    .add_node(crate::graph::NodeKind::LlmCall {
                        model: model.clone(),
                        input_tokens,
                        output_tokens,
                    });
                // Update status bar with live stats
                self.status_bar
                    .update_live_stats(model, input_tokens, output_tokens, latency_ms);
                // Render to display updated stats
                self.render_tui().await?;
            }

            ReplEvent::AgentLifecycle(event) => {
                let finished = match &event {
                    crate::runtime::scheduler::AgentEvent::TaskFinished { result } => {
                        Some(result.clone())
                    }
                    _ => None,
                };
                self.tui_renderer.lock().await.apply_agent_event(&event);
                if let Some(result) = finished {
                    let summary = if result.final_message.trim().is_empty() {
                        result.diagnostics.join("; ")
                    } else {
                        result.final_message
                    };
                    self.output_manager.write_info(format!(
                        "child {} {:?} ({} turns, {} ms)\n{}",
                        result.identity.agent_id,
                        result.status,
                        result.turns,
                        result.elapsed_ms,
                        summary
                    ));
                }
                self.render_tui().await?;
            }

            ReplEvent::CancelQuery => {
                // Get the active query ID
                let query_id = {
                    let active = self.active_query_id.read().await;
                    *active
                };

                if let Some(qid) = query_id {
                    // Fire the per-query cancellation token so handle_present_plan
                    // (and any other token-aware loops) can detect the cancel immediately.
                    self.query_states.cancel_query(qid).await;

                    // Clear active query
                    *self.active_query_id.write().await = None;
                    // Clear tool-call history for cancelled query
                    self.tool_call_history.write().await.remove(&qid);
                    if let Some(pending) = self.pending_named_brain_turns.remove(&qid) {
                        let _ = pending.response_tx.send(Err(crate::server::RunnerTurnError {
                            message: "named Brain turn cancelled by runner user".into(),
                            turn_events: pending.turn_events,
                        }));
                    }

                    // If we were in plan/executing mode, cancel that too so the
                    // user doesn't have to press Ctrl+C again to escape.
                    {
                        let mode = self.mode.read().await.clone();
                        if !matches!(mode, ReplMode::Normal) {
                            *self.mode.write().await = ReplMode::Normal;
                            self.update_plan_mode_indicator(&ReplMode::Normal);
                        }
                    }

                    // Show cancellation message
                    self.output_manager
                        .write_info("⚠️  Query cancelled by user (Ctrl+C)");
                    self.render_tui().await?;

                    tracing::info!("Query {} cancelled by user", qid);
                } else {
                    // No active query — Ctrl+C when idle:
                    //   • in plan/executing mode → exit that mode, stay in finch
                    //   • in normal mode → exit finch entirely (like Ctrl+D or /quit)
                    let mode = self.mode.read().await.clone();
                    if !matches!(mode, ReplMode::Normal) {
                        *self.mode.write().await = ReplMode::Normal;
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan mode cancelled (Ctrl+C).");
                        self.render_tui().await?;
                    } else {
                        let _ = self.event_tx.send(ReplEvent::Shutdown);
                    }
                }
            }

            ReplEvent::Shutdown => {
                // Handled in run() method - this should not be reached
                unreachable!("Shutdown event should be handled in run() method");
            }

            ReplEvent::PosetComplete { result } => {
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {
                        self.output_manager.write_info("📚 Program complete.");
                    }
                    Err(e) => {
                        self.output_manager.write_info(format!("📚 Error: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::LispResult { result } => {
                match result {
                    Ok(text) if text != "()" && !text.is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.output_manager.write_info(format!("lisp: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::RemoteBrainMessage { target, message } => {
                let is_current = self.selected_brain_matches(&target);
                if is_current {
                    let acknowledged_seq = match &message {
                        crate::brain::shared::BrainWireMessage::Snapshot { brain } => {
                            brain.revision
                        }
                        crate::brain::shared::BrainWireMessage::Event { event } => event.seq,
                    };
                    self.render_remote_brain_message(message).await?;
                    if let Some(client) = self.selected_brain_mut() {
                        if let Err(error) = client.acknowledge(acknowledged_seq).await {
                            self.output_manager
                                .write_info(format!("{target}: could not save cursor: {error}"));
                            self.render_tui().await?;
                        }
                    }
                }
            }
            ReplEvent::RemoteBrainError { target, error } => {
                self.output_manager.write_info(format!("{target}: {error}"));
                self.render_tui().await?;
            }
            ReplEvent::RemoteBrainDisconnected { target } => {
                let is_current = self.selected_brain_matches(&target);
                if is_current {
                    self.clear_remote_brain_approvals_for_target(&target).await;
                    let role = if self.selected_brain_is_home() {
                        "home"
                    } else {
                        "driver"
                    };
                    self.status_bar.update_line(
                        crate::cli::status_bar::StatusLineType::SessionLabel,
                        format!("◆ brain: {target} · {role} · disconnected"),
                    );
                    self.output_manager.write_info(format!(
                        "{target}: Brain event connection closed; detach or reattach to reconnect"
                    ));
                    self.render_tui().await?;
                }
            }
            ReplEvent::RunnerLeaseStatus {
                brain,
                epoch,
                lease_id,
                detail,
            } => {
                if epoch
                    != self
                        .runner_renewal_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Ok(());
                }
                let registration = match (lease_id, self.ipc_client.as_ref()) {
                    (Some(lease_id), Some(ipc)) => match ipc
                        .register_brain_runner(
                            &brain,
                            lease_id,
                            self.event_tx.clone(),
                        )
                        .await
                    {
                        Ok(bootstrap) => self
                            .program_runtime
                            .hydrate_reducible_state_if_newer(
                                bootstrap.checkpoint,
                                bootstrap.runtime_revision,
                            )
                            .await
                            .map(|_| ())
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error.to_string()),
                    },
                    (Some(_), None) => Err("Cap'n Proto daemon connection unavailable".into()),
                    (None, _) => Err(detail.clone()),
                };
                let active = registration.is_ok();
                self.home_runner_lease_active = active;
                self.home_runner_lease_id = if active { lease_id } else { None };
                self.runner_brain = active.then_some(brain.clone());
                let registration_error = registration.err();
                if self.active_remote_brain.is_none() {
                    if self.home_brain.is_some() {
                        self.update_remote_brain_status(active);
                    } else {
                        self.status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::SessionLabel,
                            if active {
                                format!("◆ brain: {} · runner", brain)
                            } else {
                                format!("◆ brain: {} · home · no runner lease", self.session_label)
                            },
                        );
                    }
                    if let Some(error) = registration_error {
                        let changed = self.last_home_runner_error.as_deref() != Some(&error);
                        self.last_home_runner_error = Some(error.clone());
                        if changed {
                            self.output_manager.write_info(format!(
                                "{}: runner unavailable: {}",
                                self.session_label, error
                            ));
                        }
                    } else {
                        self.last_home_runner_error = None;
                    }
                    self.render_tui().await?;
                }
            }
            ReplEvent::NamedBrainProgramRequested(request) => {
                self.dispatch_named_brain_program(request);
            }
            ReplEvent::NamedBrainTurnRequested(request) => {
                self.dispatch_named_brain_turn(request).await?;
            }
            ReplEvent::NamedBrainRunCancelRequested(request) => {
                self.cancel_named_brain_run(request).await;
            }
            ReplEvent::NamedBrainProgramFinished(run_id) => {
                self.pending_named_brain_programs.remove(&run_id);
            }
            ReplEvent::ShowDialog {
                dialog: _,
                response_tx,
            } => {
                // active_dialog is already set by the caller (belt-and-suspenders in
                // handle_present_plan / handle_ask_user_question), so the dialog is
                // on-screen before the event is even enqueued — no race window.
                // Just store the response channel for the render tick to route the result.
                self.pending_dialog_tx = Some(response_tx);
            }
        }

        Ok(())
    }

    fn dispatch_named_brain_program(&mut self, request: crate::server::RunnerProgramRequest) {
        if self.runner_brain.as_deref() != Some(request.brain.as_str())
            || !self.home_runner_lease_active
        {
            let _ = request.response_tx.send(Err(format!(
                "frontend does not hold the runner lease for named Brain '{}'",
                request.brain
            )));
            return;
        }
        let runtime = Arc::clone(&self.program_runtime);
        let event_tx = self.event_tx.clone();
        let run_id = request.run_id;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.pending_named_brain_programs
            .insert(run_id, cancel.clone());
        tokio::spawn(async move {
            let language = match request.language {
                crate::brain::shared::ProgramLanguage::Forth => {
                    crate::programs::ProgramLanguage::Forth
                }
                crate::brain::shared::ProgramLanguage::Lisp => {
                    crate::programs::ProgramLanguage::Lisp
                }
            };
            let submission = crate::runtime::ProgramSubmission {
                language,
                source_id: Some(format!(
                    "brain:{}:event:{}",
                    request.brain, request.request_seq
                )),
                source: request.source,
                intent: format!("named Brain program event {}", request.request_seq),
                effect: crate::programs::ExecutionEffect::Unclassified,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            };
            let execution = async {
                let outcome = runtime.submit_typed_only(submission).await?;
                let outcome = super::query_processor::resume_interactive_boundaries(
                    runtime.as_ref(),
                    event_tx.clone(),
                    outcome,
                )
                .await?;
                if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
                    anyhow::bail!(
                        "named Brain ProgramRun ended as {:?}: {}",
                        outcome.status,
                        outcome.diagnostics.join("; ")
                    );
                }
                let checkpoint = runtime
                    .revision_history()?
                    .into_iter()
                    .find(|snapshot| snapshot.revision == outcome.output_revision)
                    .and_then(|snapshot| snapshot.checkpoint)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "named Brain revision {} is not checkpointable",
                            outcome.output_revision
                        )
                    })?;
                Ok(crate::server::RunnerProgramResult {
                    output: outcome.output,
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                })
            };
            let result = tokio::select! {
                _ = cancel.cancelled() => Err(anyhow::anyhow!("named Brain run cancelled")),
                result = execution => result,
            }
            .map_err(|error: anyhow::Error| error.to_string());
            let _ = request.response_tx.send(result);
            let _ = event_tx.send(ReplEvent::NamedBrainProgramFinished(run_id));
        });
    }

    async fn dispatch_named_brain_turn(
        &mut self,
        request: crate::server::RunnerTurnRequest,
    ) -> Result<()> {
        if self.runner_brain.as_deref() != Some(request.brain.as_str())
            || !self.home_runner_lease_active
        {
            let _ = request.response_tx.send(Err(crate::server::RunnerTurnError {
                message: format!(
                    "frontend does not hold the runner lease for named Brain '{}'",
                    request.brain
                ),
                turn_events: Vec::new(),
            }));
            return Ok(());
        }
        if self.active_query_id.read().await.is_some() {
            let _ = request.response_tx.send(Err(crate::server::RunnerTurnError {
                message: format!(
                    "named Brain '{}' runner is already executing a turn",
                    request.brain
                ),
                turn_events: Vec::new(),
            }));
            return Ok(());
        }

        let mut context = request.context;
        if context.is_empty() {
            context.push(crate::claude::Message::user(request.prompt.clone()));
        }
        self.conversation
            .write()
            .await
            .restore_snapshot(context.clone());
        let query_id = self.query_states.create_query(context).await;
        *self.active_query_id.write().await = Some(query_id);
        self.pending_named_brain_turns.insert(
            query_id,
            PendingNamedBrainTurn {
                run_id: request.run_id,
                response_tx: request.response_tx,
                turn_events: Vec::new(),
                approval_audience: request.approval_audience,
                approval_tx: request.approval_tx,
            },
        );
        self.update_compaction_status().await;
        if self
            .llm_tx
            .send(LlmRequest::Query {
                id: query_id,
                text: request.prompt,
                no_tools: false,
            })
            .is_err()
        {
            *self.active_query_id.write().await = None;
            if let Some(pending) = self.pending_named_brain_turns.remove(&query_id) {
                let _ = pending.response_tx.send(Err(crate::server::RunnerTurnError {
                    message: "frontend LLM worker is unavailable".to_string(),
                    turn_events: pending.turn_events,
                }));
            }
        }
        Ok(())
    }

    async fn cancel_named_brain_run(&mut self, request: crate::server::RunnerCancelRequest) {
        if self.runner_brain.as_deref() != Some(request.brain.as_str())
            || !self.home_runner_lease_active
        {
            let _ = request.response_tx.send(Err(format!(
                "frontend does not hold the runner lease for named Brain '{}'",
                request.brain
            )));
            return;
        }
        if let Some(cancel) = self.pending_named_brain_programs.get(&request.run_id) {
            cancel.cancel();
            let _ = request.response_tx.send(Ok(true));
            return;
        }
        let query_id = self.pending_named_brain_turns.iter().find_map(|(query_id, turn)| {
            (turn.run_id == request.run_id).then_some(*query_id)
        });
        let Some(query_id) = query_id else {
            let _ = request.response_tx.send(Ok(false));
            return;
        };
        self.query_states.cancel_query(query_id).await;
        if *self.active_query_id.read().await == Some(query_id) {
            *self.active_query_id.write().await = None;
        }
        self.tool_call_history.write().await.remove(&query_id);
        if let Some(pending) = self.pending_named_brain_turns.remove(&query_id) {
            let _ = pending.response_tx.send(Err(crate::server::RunnerTurnError {
                message: "named Brain run cancelled".into(),
                turn_events: pending.turn_events,
            }));
        }
        let _ = request.response_tx.send(Ok(true));
    }

    fn finish_named_brain_turn(&mut self, query_id: Uuid, output: String) {
        let Some(pending) = self.pending_named_brain_turns.remove(&query_id) else {
            return;
        };
        let PendingNamedBrainTurn {
            response_tx,
            turn_events,
            ..
        } = pending;
        let result = (|| -> anyhow::Result<crate::server::RunnerTurnResult> {
            let messages = self
                .conversation
                .try_read()
                .map_err(|_| anyhow::anyhow!("named Brain conversation is busy"))?
                .get_messages();
            let source = messages
                .into_iter()
                .rev()
                .find(|message| message.role == "assistant")
                .map(|message| message.text())
                .filter(|source| !source.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("named Brain turn produced no wire source"))?;
            let language = match crate::programs::ProgramLanguage::infer_wire_source(&source)? {
                crate::programs::ProgramLanguage::Forth => {
                    crate::brain::shared::ProgramLanguage::Forth
                }
                crate::programs::ProgramLanguage::Lisp => {
                    crate::brain::shared::ProgramLanguage::Lisp
                }
            };
            let runtime_revision = self.program_runtime.revision();
            let checkpoint = self
                .program_runtime
                .revision_history()?
                .into_iter()
                .find(|snapshot| snapshot.revision == runtime_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "named Brain revision {runtime_revision} is not checkpointable"
                    )
                })?;
            Ok(crate::server::RunnerTurnResult {
                source,
                language,
                output,
                turn_events: turn_events.clone(),
                runtime_revision,
                checkpoint,
            })
        })()
        .map_err(|error| crate::server::RunnerTurnError {
            message: error.to_string(),
            turn_events,
        });
        if let Ok(result) = &result {
            let tool_ids = result
                .turn_events
                .iter()
                .filter_map(|event| match event {
                    crate::server::RunnerTurnEvent::Call { tool_id, .. }
                    | crate::server::RunnerTurnEvent::Result { tool_id, .. } => {
                        Some(tool_id.clone())
                    }
                    crate::server::RunnerTurnEvent::ApprovalRequested { .. }
                    | crate::server::RunnerTurnEvent::ApprovalDecided { .. } => None,
                })
                .collect();
            let approval_ids = result
                .turn_events
                .iter()
                .filter_map(|event| match event {
                    crate::server::RunnerTurnEvent::ApprovalRequested { approval_id, .. }
                    | crate::server::RunnerTurnEvent::ApprovalDecided { approval_id, .. } => {
                        Some(approval_id.clone())
                    }
                    crate::server::RunnerTurnEvent::Call { .. }
                    | crate::server::RunnerTurnEvent::Result { .. } => None,
                })
                .collect();
            self.local_brain_projections.push_back(LocalBrainProjection {
                source: result.source.clone(),
                output: result.output.clone(),
                tool_ids,
                approval_ids,
                program_seq: None,
            });
        }
        let _ = response_tx.send(result);
    }

    /// The editor runs outside the VM; once it finishes, resume precisely the
    /// saved effect rather than resubmitting source or replaying prior output.
    fn spawn_deferred_proposal(
        &self,
        query_id: Uuid,
        tool_id: String,
        proposal: DeferredProposal,
        approval_audience: Option<crate::brain::shared::BrainApprovalAudience>,
    ) {
        let event_tx = self.event_tx.clone();
        let runtime = Arc::clone(&self.program_runtime);
        tokio::spawn(async move {
            let result = async {
                let intent = approval_audience
                    .as_ref()
                    .map(|audience| {
                        format!(
                            "{}\n\n{}",
                            proposal.intent,
                            approval_audience_summary(audience)
                        )
                    })
                    .unwrap_or_else(|| proposal.intent.clone());
                let decision = crate::tools::implementations::propose::propose_artifact_with_decision(
                    &proposal.language,
                    &intent,
                    &proposal.source,
                )
                .await?;
                let outcome = resume_deferred_proposal(runtime.as_ref(), &proposal, decision).await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&outcome)?)
            }
            .await;
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            });
        });
    }

    /// Resolve a capability prompt emitted through provider-native
    /// `submit_program`, then return the resumed outcome through the original
    /// tool-result lifecycle. A later capability boundary naturally repeats
    /// this process with its own prompt and sequence.
    fn spawn_deferred_vm_approval(
        &self,
        query_id: Uuid,
        tool_id: String,
        approval: DeferredVmApproval,
    ) {
        let event_tx = self.event_tx.clone();
        let runtime = Arc::clone(&self.program_runtime);
        tokio::spawn(async move {
            let result = async {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                event_tx
                    .send(ReplEvent::VmApprovalNeeded {
                        prompt: approval.prompt.clone(),
                        response_tx,
                    })
                    .map_err(|_| anyhow::anyhow!("VM approval UI is unavailable"))?;
                let choice = response_rx
                    .await
                    .map_err(|_| anyhow::anyhow!("VM approval dialog was cancelled"))?;
                let outcome = runtime
                    .resolve_typed_approval(
                        &approval.prompt,
                        choice,
                        "interactive-tool-user",
                    )
                    .await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&outcome)?)
            }
            .await;
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            });
        });
    }

    // ── Diff proposal rendering ───────────────────────────────────────────────

    /// Render a diff proposal visually in the room output.
    ///
    /// ```text
    /// peer-a1b2c3d4 proposes: src/session/mod.rs
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// - old line
    /// + new line
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// "adds Diff variant to SessionEvent"
    /// [accept: /accept <id>] [reject: /reject <reason>]
    /// ```
    fn render_diff_proposal(
        &self,
        id: uuid::Uuid,
        label: &str,
        patch: &str,
        description: Option<&str>,
        proposed_by: &str,
    ) {
        use crossterm::style::Stylize;
        const BAR: &str = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━";

        let short_id = &id.to_string()[..8];

        // Header
        let id_tag = format!("[id: {}]", short_id);
        self.output_manager.write_info(format!(
            "{} proposes: {}  {}",
            proposed_by.cyan(),
            label.white().bold(),
            id_tag.as_str().dark_grey(),
        ));
        self.output_manager.write_info(BAR.dark_grey().to_string());

        // Patch lines — colour additions green, removals red
        for line in patch.lines() {
            let rendered = if line.starts_with('+') && !line.starts_with("+++") {
                line.green().to_string()
            } else if line.starts_with('-') && !line.starts_with("---") {
                line.red().to_string()
            } else if line.starts_with("@@") {
                line.cyan().to_string()
            } else {
                line.dark_grey().to_string()
            };
            self.output_manager.write_info(rendered);
        }

        self.output_manager.write_info(BAR.dark_grey().to_string());

        // Optional description
        if let Some(desc) = description {
            self.output_manager
                .write_info(format!("  \"{}\"", desc.dark_grey()));
        }

        // Action hints
        self.output_manager.write_info(format!(
            "  {}  {}",
            format!("[accept: /accept {}]", short_id).as_str().green(),
            "[reject: /reject <reason>]".dark_grey(),
        ));
    }

    /// Handle `/accept [prefix]` — apply the patch and notify peers.
    async fn handle_accept(&mut self, prefix: Option<String>) -> Result<()> {
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

        // Broadcast DiffAccept so the proposing peer knows
        let _ = self
            .peer_tx
            .send(crate::session::SessionEvent::diff_accept(diff_id));
        self.render_tui().await
    }

    /// Apply a unified diff patch to a file on disk.
    ///
    /// This is a simple line-based applicator that handles the most common
    /// unified diff format (`--- a/file`, `+++ b/file`, `@@ ... @@` hunks).
    /// It is not a full POSIX patch implementation — it is good enough for
    /// AI-proposed diffs that the AI has computed against the current file.
    fn apply_unified_diff(&self, label: &str, patch: &str) -> anyhow::Result<()> {
        use std::path::Path;

        // Extract the target filename from the patch's `+++ b/...` line,
        // falling back to `label` if not found.
        let target_path = patch
            .lines()
            .find(|l| l.starts_with("+++ "))
            .and_then(|l| {
                let s = l.trim_start_matches("+++ ");
                // Strip `b/` prefix if present
                let s = s.strip_prefix("b/").unwrap_or(s);
                // Strip timestamp suffix (a tab followed by date)
                let s = s.split('\t').next().unwrap_or(s);
                if s == "/dev/null" {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .unwrap_or_else(|| label.to_string());

        let path = Path::new(&target_path);

        // Read original file (empty if it doesn't exist — new-file diff)
        let original: Vec<String> = if path.exists() {
            std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("could not read {}: {}", target_path, e))?
                .lines()
                .map(|l| l.to_string())
                .collect()
        } else {
            Vec::new()
        };

        let patched = apply_patch_lines(&original, patch)
            .map_err(|e| anyhow::anyhow!("patch failed: {}", e))?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("could not create dirs: {}", e))?;
            }
        }
        std::fs::write(path, patched.join("\n") + "\n")
            .map_err(|e| anyhow::anyhow!("could not write {}: {}", target_path, e))?;

        Ok(())
    }

    /// Handle `/reject [reason]` — reject the most recent pending diff and notify peers.
    async fn handle_reject(&mut self, reason: Option<String>) -> Result<()> {
        use crossterm::style::Stylize;

        let diff_id = {
            let d = self.diff_store.resolve_pending(None);
            match d {
                None => {
                    self.output_manager
                        .write_info("no pending diff to reject".dark_grey().to_string());
                    return self.render_tui().await;
                }
                Some(d) => d.id,
            }
        };

        self.diff_store.reject(diff_id, reason.clone());
        let reason_str = reason.clone().unwrap_or_default();
        self.output_manager.write_info(format!(
            "{}  diff {} rejected{}",
            "✗".red(),
            &diff_id.to_string()[..8].white(),
            if reason_str.is_empty() {
                String::new()
            } else {
                format!(": {}", reason_str)
            },
        ));

        // Broadcast DiffReject so the proposing peer knows
        let _ = self
            .peer_tx
            .send(crate::session::SessionEvent::diff_reject(diff_id, reason));
        self.render_tui().await
    }

    /// Attach this TUI to one daemon-owned brain. All prompts and explicit
    /// Forth/Lisp programs are routed to that host until `/brain detach`.
    async fn handle_brain_attach(&mut self, value: String) -> Result<()> {
        let parsed = if value.contains('@') {
            crate::brain::remote::RemoteBrainTarget::parse(&value)
        } else if let Some(base) = self.daemon_base_url.as_deref() {
            crate::brain::remote::RemoteBrainTarget::local(&value, base)
        } else {
            Err(anyhow::anyhow!(
                "a bare Brain name requires a connected local daemon; use NAME@MACHINE[:PORT]"
            ))
        };
        let target = match parsed {
            Ok(target) => target,
            Err(error) => {
                self.output_manager
                    .write_info(format!("brain attach: {error}"));
                self.render_tui().await?;
                return Ok(());
            }
        };
        if self.home_brain.as_ref().is_some_and(|home| {
            home.target.brain == target.brain && home.target.address == target.address
        }) {
            self.output_manager.write_info(format!(
                "{} is already this console's home Brain",
                target.display_name()
            ));
            return self.render_tui().await;
        }
        let password = crate::config::load_config()
            .map(|config| config.server.brain_password)
            .unwrap_or_default();
        let remote = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        let mut client = crate::brain::remote::AttachedBrainClient::remote(remote);
        if let Err(error) = client
            .attach_persistent(
                &self.participant_subject,
                crate::brain::shared::AttachmentRole::Driver,
                &self.session_label,
            )
            .await
        {
            self.output_manager.write_info(format!(
                "brain attach {}: {error}",
                client.target.display_name()
            ));
            self.render_tui().await?;
            return Ok(());
        }
        let mut incoming = match client.watch().await {
            Ok(incoming) => incoming,
            Err(error) => {
                let _ = client.disconnect().await;
                self.output_manager.write_info(format!(
                    "brain attach {}: {error}",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };
        let snapshot = match incoming.recv().await {
            Some(crate::brain::shared::BrainWireMessage::Snapshot { brain }) => brain,
            Some(crate::brain::shared::BrainWireMessage::Event { .. }) => {
                self.output_manager.write_info(format!(
                    "brain attach {}: event stream did not begin with a snapshot",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
            None => {
                self.output_manager.write_info(format!(
                    "brain attach {}: event stream closed before its snapshot",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };
        client.target.machine = snapshot.environment.machine.clone();

        let target_name = client.target.display_name();
        let runner_online = snapshot.runner_lease.is_some();
        self.active_remote_brain = Some(client);
        self.update_remote_brain_status(runner_online);
        self.render_remote_brain_message(crate::brain::shared::BrainWireMessage::Snapshot {
            brain: snapshot,
        })
        .await?;

        let event_tx = self.event_tx.clone();
        tokio::task::spawn_local(async move {
            while let Some(message) = incoming.recv().await {
                if event_tx
                    .send(ReplEvent::RemoteBrainMessage {
                        target: target_name.clone(),
                        message,
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = event_tx.send(ReplEvent::RemoteBrainDisconnected {
                target: target_name,
            });
        });
        Ok(())
    }

    async fn handle_brain_detach(&mut self) -> Result<()> {
        if let Some(client) = self.active_remote_brain.take() {
            self.clear_remote_brain_approvals_for_target(&client.target.display_name())
                .await;
            if let Err(error) = client.disconnect().await {
                self.output_manager.write_info(format!(
                    "{}: could not close attachment cleanly: {error}",
                    client.target.display_name()
                ));
            }
            self.output_manager
                .write_info(format!("detached from {}", client.target.display_name()));
        }
        if let Some(home) = self.home_brain.as_ref() {
            let snapshot = home.snapshot().await?;
            self.render_remote_brain_message(
                crate::brain::shared::BrainWireMessage::Snapshot {
                    brain: snapshot.clone(),
                },
            )
            .await?;
            if let Some(home) = self.home_brain.as_mut() {
                home.acknowledge(snapshot.revision).await?;
            }
        } else {
            self.status_bar.update_line(
                crate::cli::status_bar::StatusLineType::SessionLabel,
                if self.home_runner_lease_active {
                    format!("◆ brain: {} · runner", self.session_label)
                } else {
                    format!("◆ brain: {} · home · no runner lease", self.session_label)
                },
            );
        }
        self.render_tui().await
    }

    /// Show or rotate the credential only on the execution environment's local daemon.
    async fn handle_brain_password(&mut self, password: Option<String>) -> Result<()> {
        if self.active_remote_brain.is_some() {
            self.output_manager
                .write_info("brain password is visible only from the brain's execution host");
            return self.render_tui().await;
        }
        let base = self
            .daemon_base_url
            .clone()
            .unwrap_or_else(|| format!("http://{}", crate::config::constants::DEFAULT_HTTP_ADDR));
        let http = reqwest::Client::new();
        match password {
            Some(password) => {
                let response = http
                    .put(format!("{base}/v1/brains/password"))
                    .json(&serde_json::json!({"password": password}))
                    .send()
                    .await?;
                if response.status().is_success() {
                    self.output_manager.write_info("brain password updated");
                } else {
                    self.output_manager.write_info(format!(
                        "brain password update failed: {}",
                        response.text().await.unwrap_or_default()
                    ));
                }
            }
            None => {
                let response = http
                    .get(format!("{base}/v1/brains/password"))
                    .send()
                    .await?;
                if response.status().is_success() {
                    let body: serde_json::Value = response.json().await?;
                    self.output_manager.write_info(format!(
                        "brain password: {}",
                        body["password"].as_str().unwrap_or("<not configured>")
                    ));
                } else {
                    self.output_manager
                        .write_info("brain password is unavailable from this client");
                }
            }
        }
        self.render_tui().await
    }

    fn selected_brain(&self) -> Option<&crate::brain::remote::AttachedBrainClient> {
        self.active_remote_brain
            .as_ref()
            .or(self.home_brain.as_ref())
    }

    fn selected_brain_mut(&mut self) -> Option<&mut crate::brain::remote::AttachedBrainClient> {
        self.active_remote_brain
            .as_mut()
            .or(self.home_brain.as_mut())
    }

    fn selected_brain_is_home(&self) -> bool {
        self.active_remote_brain.is_none() && self.home_brain.is_some()
    }

    fn selected_brain_matches(&self, target: &str) -> bool {
        self.selected_brain()
            .is_some_and(|client| client.target.display_name() == target)
    }

    async fn push_remote_brain(
        &mut self,
        kind: crate::brain::shared::BrainEventKind,
    ) -> Result<()> {
        let Some(client) = self.selected_brain().cloned() else {
            return Ok(());
        };
        let target = client.target.display_name();
        let event_tx = self.event_tx.clone();
        tokio::task::spawn_local(async move {
            if let Err(error) = client.push(kind).await {
                let _ = event_tx.send(ReplEvent::RemoteBrainError {
                    target,
                    error: error.to_string(),
                });
            }
        });
        Ok(())
    }

    async fn render_remote_brain_message(
        &mut self,
        message: crate::brain::shared::BrainWireMessage,
    ) -> Result<()> {
        match message {
            crate::brain::shared::BrainWireMessage::Snapshot { brain } => {
                self.update_remote_brain_status(brain.runner_lease.is_some());
                let acknowledged_seq = self
                    .selected_brain()
                    .and_then(|client| client.attachment())
                    .map(|attachment| attachment.acknowledged_seq)
                    .unwrap_or(0);
                self.output_manager.write_info(format!(
                    "environment: {}:{} (generation {})",
                    brain.environment.machine,
                    brain.environment.workspace.display(),
                    brain.environment.generation,
                ));
                for event in brain
                    .events
                    .iter()
                    .filter(|event| event.seq > acknowledged_seq)
                {
                    self.render_remote_brain_event(event);
                    self.observe_remote_brain_approval(event);
                }
            }
            crate::brain::shared::BrainWireMessage::Event { event } => {
                match &event.kind {
                    crate::brain::shared::BrainEventKind::RunnerLeaseAcquired { .. } => {
                        self.update_remote_brain_status(true);
                    }
                    crate::brain::shared::BrainEventKind::RunnerLeaseReleased { .. } => {
                        self.update_remote_brain_status(false);
                    }
                    _ => {}
                }
                self.render_remote_brain_event(&event);
                self.observe_remote_brain_approval(&event);
            }
        }
        self.try_present_remote_brain_approval().await?;
        self.render_tui().await
    }

    fn observe_remote_brain_approval(&mut self, event: &crate::brain::shared::BrainEvent) {
        use crate::brain::shared::BrainEventKind;

        match &event.kind {
            BrainEventKind::ApprovalRequested {
                request_seq,
                approval_id,
                approval_kind,
                subject,
                audience: Some(audience),
                detail,
            } => {
                let Some(client) = self.selected_brain().cloned() else {
                    return;
                };
                if client
                    .attachment()
                    .is_none_or(|attachment| attachment.attachment_id != audience.attachment_id)
                {
                    return;
                }
                if self
                    .active_remote_brain_approval
                    .as_ref()
                    .is_some_and(|pending| pending.approval_id == *approval_id)
                    || self
                        .queued_remote_brain_approvals
                        .iter()
                        .any(|pending| pending.approval_id == *approval_id)
                {
                    return;
                }
                let kind = match approval_kind.as_str() {
                    "tool" => RemoteBrainApprovalKind::Tool(crate::tools::types::ToolUse {
                        id: approval_id.clone(),
                        name: subject.clone(),
                        input: detail
                            .get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    }),
                    "vm_capability" => {
                        let Ok(prompt) = serde_json::from_value::<crate::vm::ApprovalPrompt>(
                            detail.clone(),
                        ) else {
                            self.output_manager.write_error(format!(
                                "approval {approval_id} has an invalid VM capability prompt"
                            ));
                            return;
                        };
                        let choices = vm_approval_choices(&prompt);
                        RemoteBrainApprovalKind::Vm { prompt, choices }
                    }
                    _ => {
                        self.output_manager.write_error(format!(
                            "approval {approval_id} has unknown kind '{approval_kind}'"
                        ));
                        return;
                    }
                };
                self.queued_remote_brain_approvals
                    .push_back(RemoteBrainApproval {
                        client,
                        request_seq: *request_seq,
                        approval_id: approval_id.clone(),
                        audience: audience.clone(),
                        kind,
                    });
            }
            BrainEventKind::ApprovalDecided { approval_id, .. } => {
                self.queued_remote_brain_approvals
                    .retain(|pending| pending.approval_id != *approval_id);
                if self
                    .active_remote_brain_approval
                    .as_ref()
                    .is_some_and(|pending| pending.approval_id == *approval_id)
                {
                    self.active_remote_brain_approval = None;
                }
            }
            _ => {}
        }
    }

    async fn try_present_remote_brain_approval(&mut self) -> Result<()> {
        if self.active_remote_brain_approval.is_some()
            || self.queued_remote_brain_approvals.is_empty()
        {
            return Ok(());
        }
        let mut tui = self.tui_renderer.lock().await;
        if tui.active_dialog.is_some() {
            return Ok(());
        }
        let pending = self
            .queued_remote_brain_approvals
            .pop_front()
            .expect("remote approval queue checked above");
        let dialog = match &pending.kind {
            RemoteBrainApprovalKind::Tool(tool_use) => {
                let mut summary = tool_approval_summary(tool_use);
                summary.push_str("\n\n");
                summary.push_str(&approval_audience_summary(&pending.audience));
                crate::cli::tui::Dialog::tool_approval(&tool_use.name, &summary)
            }
            RemoteBrainApprovalKind::Vm { prompt, .. } => {
                vm_approval_dialog(prompt, Some(&pending.audience), self.program_runtime.as_ref())
            }
        };
        self.active_remote_brain_approval = Some(pending);
        tui.active_dialog = Some(dialog);
        tui.pending_dialog_result = None;
        tui.render()?;
        Ok(())
    }

    async fn clear_remote_brain_approvals_for_target(&mut self, target: &str) {
        self.queued_remote_brain_approvals
            .retain(|pending| pending.client.target.display_name() != target);
        let clear_dialog = self
            .active_remote_brain_approval
            .as_ref()
            .is_some_and(|pending| pending.client.target.display_name() == target);
        if clear_dialog {
            self.active_remote_brain_approval = None;
            let mut tui = self.tui_renderer.lock().await;
            tui.active_dialog = None;
            tui.pending_dialog_result = None;
        }
    }

    fn update_remote_brain_status(&self, runner_online: bool) {
        let Some(client) = self.selected_brain() else {
            return;
        };
        let role = client
            .attachment()
            .map(|attachment| format!("{:?}", attachment.role).to_lowercase())
            .unwrap_or_else(|| "detached".into());
        let role = if self.home_runner_lease_active
            && self.runner_brain.as_deref() == Some(client.target.brain.as_str())
        {
            format!("runner · {role}")
        } else {
            format!(
                "{role} · runner {}",
                if runner_online { "online" } else { "offline" }
            )
        };
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            format!("◆ brain: {} · {role}", client.target.display_name()),
        );
    }

    fn render_remote_brain_event(&mut self, event: &crate::brain::shared::BrainEvent) {
        use crate::brain::shared::BrainEventKind;
        match &event.kind {
            BrainEventKind::RunnerLeaseAcquired { lease } => self.output_manager.write_info(
                format!("{} is the active environment runner", lease.subject),
            ),
            BrainEventKind::RunnerLeaseReleased { .. } => self
                .output_manager
                .write_info("environment runner disconnected"),
            BrainEventKind::RunnerHandoffRequested { handoff } => {
                let prompt = if handoff.target_subject == self.runner_subject {
                    format!(
                        "; addressed to this frontend — use /brain handoff accept {}",
                        &handoff.handoff_id.0.to_string()[..8]
                    )
                } else {
                    String::new()
                };
                self.output_manager.write_info(format!(
                    "{} requested runner handoff to {}{}",
                    handoff.requested_by, handoff.target_subject, prompt
                ));
            }
            BrainEventKind::RunnerHandoffCompleted { lease, .. } => self
                .output_manager
                .write_info(format!("runner handoff completed to {}", lease.subject)),
            BrainEventKind::RunnerHandoffCancelled { .. } => self
                .output_manager
                .write_info("runner handoff cancelled"),
            BrainEventKind::ClientAttached {
                subject, role, ..
            } => self.output_manager.write_info(format!(
                "{subject} attached as {}",
                format!("{role:?}").to_lowercase()
            )),
            BrainEventKind::ClientDetached { attachment_id, .. } => self
                .output_manager
                .write_info(format!("attachment {} disconnected", attachment_id.0)),
            BrainEventKind::RunStarted { run }
                if run.status
                    == crate::brain::shared::BrainRunStatus::QueuedForEnvironment =>
            {
                let run_id = run.run_id.0.to_string();
                self.output_manager.write_info(format!(
                    "run {} queued for the environment runner",
                    &run_id[..8]
                ));
            }
            BrainEventKind::RunStatusChanged {
                run_id,
                status: crate::brain::shared::BrainRunStatus::Running,
                ..
            } => {
                let run_id = run_id.0.to_string();
                self.output_manager
                    .write_info(format!("run {} resumed on the environment runner", &run_id[..8]));
            }
            BrainEventKind::RunStatusChanged {
                run_id,
                status: crate::brain::shared::BrainRunStatus::Interrupted,
                detail,
            } => {
                let run_id = run_id.0.to_string();
                self.output_manager.write_info(format!(
                    "run {} interrupted{}",
                    &run_id[..8],
                    detail
                        .as_deref()
                        .map(|detail| format!(": {detail}"))
                        .unwrap_or_default()
                ));
            }
            BrainEventKind::RunStarted { .. } | BrainEventKind::RunStatusChanged { .. } => {}
            BrainEventKind::Prompt { text } => self
                .output_manager
                .write_user(format!("{}: {text}", event.sender)),
            BrainEventKind::ParticipantMessage { text } => self
                .output_manager
                .write_info(format!("{}: {text}", event.sender)),
            BrainEventKind::ToolCall {
                tool_id,
                name,
                input,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let input = input.to_string();
                let input = if input.chars().count() > 80 {
                    format!("{}…", input.chars().take(79).collect::<String>())
                } else {
                    input
                };
                let row = unit.add_row(format!("{name} {input}"));
                self.remote_brain_tool_rows.insert(tool_id.clone(), row);
            }
            BrainEventKind::ToolResult {
                tool_id,
                output,
                is_error,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let row = self
                    .remote_brain_tool_rows
                    .remove(tool_id)
                    .unwrap_or_else(|| unit.add_row(tool_id));
                if *is_error {
                    unit.fail_row(row, output);
                } else {
                    let first = output.lines().next().unwrap_or_default();
                    let summary = if first.chars().count() > 80 {
                        format!("{}…", first.chars().take(79).collect::<String>())
                    } else {
                        first.to_string()
                    };
                    let body = output.lines().skip(1).map(str::to_owned).collect();
                    unit.complete_row_with_body(row, summary, body);
                }
            }
            BrainEventKind::ApprovalRequested {
                approval_id,
                approval_kind,
                subject,
                audience,
                detail,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let audience_summary = audience
                    .as_ref()
                    .map(|audience| {
                        format!(
                            "{} ({:?}, environment {})",
                            audience.subject, audience.role, audience.environment_generation
                        )
                    })
                    .unwrap_or_else(|| "legacy audience unspecified".to_string());
                let row = unit.add_row(format!(
                    "approval ({approval_kind}) for {audience_summary}: {subject}"
                ));
                let body = serde_json::to_string_pretty(detail)
                    .unwrap_or_else(|_| detail.to_string())
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                for line in body {
                    unit.append_row_body_line(row, line);
                }
                self.remote_brain_approval_rows
                    .insert(approval_id.clone(), row);
            }
            BrainEventKind::ApprovalDecided {
                approval_id,
                decision,
                ..
            } => {
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let unit = self
                    .remote_brain_tool_unit
                    .get_or_insert_with(|| self.output_manager.start_work_unit("Brain tools"));
                let row = self
                    .remote_brain_approval_rows
                    .remove(approval_id)
                    .unwrap_or_else(|| unit.add_row(format!("approval {approval_id}")));
                let choice = decision
                    .get("choice")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("decided");
                let summary = format!("{choice} by {}", event.sender);
                if choice == "deny" {
                    unit.fail_row(row, &summary);
                } else {
                    unit.complete_row(row, &summary);
                }
            }
            BrainEventKind::Program { language, source } => {
                if let Some(unit) = self.remote_brain_tool_unit.take() {
                    unit.set_complete();
                }
                self.remote_brain_tool_rows.clear();
                self.remote_brain_approval_rows.clear();
                if self.selected_brain_is_home()
                    && self
                        .local_brain_projections
                        .front_mut()
                        .is_some_and(|projection| {
                            projection.observe(event) == LocalProjectionMatch::Suppress
                        })
                {
                    return;
                }
                let language = match language {
                    crate::brain::shared::ProgramLanguage::Forth => "forth",
                    crate::brain::shared::ProgramLanguage::Lisp => "lisp",
                };
                let unit = self
                    .output_manager
                    .start_work_unit(format!("{} program", event.sender));
                unit.set_program_source(language);
                unit.set_response(source);
                unit.set_complete();
            }
            BrainEventKind::ProgramPopped { program_seq } => self
                .output_manager
                .write_info(format!("{} popped program #{program_seq}", event.sender)),
            BrainEventKind::Result {
                request_seq: _,
                output,
                error,
            } => {
                let projection_match = self
                    .selected_brain_is_home()
                    .then(|| self.local_brain_projections.front_mut())
                    .flatten()
                    .map(|projection| projection.observe(event))
                    .unwrap_or(LocalProjectionMatch::None);
                if projection_match == LocalProjectionMatch::SuppressAndComplete {
                    self.local_brain_projections.pop_front();
                    return;
                }
                if let Some(error) = error {
                    self.output_manager.write_info(format!("error: {error}"));
                } else if !output.is_empty() {
                    let unit = self.output_manager.start_work_unit("Brain program output");
                    unit.set_program_output();
                    unit.set_response(output);
                    unit.set_complete();
                }
            }
            // Internal durable VM state is intentionally not rendered as a
            // chat item. The adjacent Program/Result events are its visible
            // projection.
            BrainEventKind::RuntimeCommitted { .. } => {}
        }
    }

    /// Render the TUI
    async fn render_tui(&self) -> Result<()> {
        // Skip all crossterm writes while an external editor owns the terminal.
        if crate::is_editor_active() {
            return Ok(());
        }
        let mut tui = self.tui_renderer.lock().await;

        // After returning from an external editor, call resume() to reset
        // active_rows so the TUI live area repaints from scratch.
        // enable_raw_mode() in resume() is idempotent — raw mode is already on.
        if crate::take_tui_rebuild() {
            tui.resume().ok();
        }

        // Check if recovery needed from previous render failure
        if tui.needs_full_refresh {
            tracing::info!("Performing full TUI refresh after render error");
            // Try to recover by clearing error state
            tui.needs_full_refresh = false;
            tui.last_render_error = None;
        }

        tui.flush_output_safe(&self.output_manager)?;
        // check_and_refresh handles the needs_full_refresh flag.
        // We do NOT call tui.render() here: flush_output_safe() already draws
        // when messages are committed or when the 100 ms animation interval
        // elapses.  Calling render() afterwards would erase the live area a
        // second time from the wrong cursor position, causing the "stacking
        // Channeling…" visual glitch.
        tui.check_and_refresh()?;
        Ok(())
    }

    /// Clean up old completed queries
    async fn cleanup_old_queries(&self) {
        self.query_states
            .cleanup_old_queries(Duration::from_secs(30))
            .await;
    }

    /// Update the compaction percentage in the status bar.
    /// No-op when auto_compact_enabled is false.
    async fn update_compaction_status(&self) {
        if !self.auto_compact_enabled {
            return;
        }
        let conversation = self.conversation.read().await;
        let percent_remaining = conversation.compaction_percent_remaining();

        // Format percentage (0-100%)
        let percent_display = (percent_remaining * 100.0) as u8;

        // Update status bar with compaction percentage (matches Claude Code format)
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::CompactionPercent,
            format!("Context left until auto-compact: {}%", percent_display),
        );
    }

    /// Handle a tool result
    async fn handle_tool_result(
        &mut self,
        query_id: Uuid,
        tool_id: String,
        result: Result<String>,
    ) -> Result<()> {
        // Look up the tool's WorkUnit and row index
        let (tool_name, tool_input, work_unit, row_idx) = {
            let mut map = self.active_tool_uses.write().await;
            map.remove(&tool_id).unwrap_or_else(|| {
                // Fallback: create a standalone WorkUnit for untracked tools
                let fallback = self.output_manager.start_work_unit("Tool");
                let row_idx = fallback.add_row(&tool_id);
                (tool_id.clone(), serde_json::Value::Null, fallback, row_idx)
            })
        };

        if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
            let (output, is_error) = match &result {
                Ok(output) => (output.clone(), false),
                Err(error) => (error.to_string(), true),
            };
            turn.turn_events
                .push(crate::server::RunnerTurnEvent::Result {
                    tool_id: tool_id.clone(),
                    output,
                    is_error,
                });
        }

        // Update the row in the WorkUnit with a semantic summary + optional body
        match &result {
            Ok(content) => {
                let (summary, mut body) = tool_result_to_display(&tool_name, content);
                // A provider-native VM submission is executable source, not an
                // opaque tool argument.  Preserve the exact source in the
                // scrollback row so a user can reconcile every `say` chunk and
                // diagnostic with the program that caused it.
                if tool_name == "submit_program" {
                    let language = tool_input
                        .get("language")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("inferred");
                    if let Some(source) = tool_input
                        .get("source")
                        .and_then(serde_json::Value::as_str)
                    {
                        let mut source_body = vec![format!("VM source ({language}):")];
                        source_body.extend(source.lines().map(str::to_owned));
                        source_body.append(&mut body);
                        body = source_body;
                    }
                }
                work_unit.complete_row_with_body(row_idx, summary, body);
            }
            Err(e) => {
                // Truncate very long error messages for the row display
                let err_str = e.to_string();
                let short_err = if err_str.len() > 60 {
                    format!("{}…", err_str.chars().take(57).collect::<String>())
                } else {
                    err_str
                };
                work_unit.fail_row(row_idx, short_err);
            }
        }

        // Record tool execution in the graph
        {
            let input_preview = {
                let s = tool_input.to_string();
                if s.chars().count() > 120 {
                    s.chars().take(120).collect::<String>()
                } else {
                    s
                }
            };
            let (output_preview, is_error) = match &result {
                Ok(c) => {
                    let preview = c.chars().take(200).collect::<String>();
                    (preview, false)
                }
                Err(e) => (e.to_string().chars().take(200).collect(), true),
            };
            self.current_graph
                .lock()
                .await
                .add_node(crate::graph::NodeKind::ToolExecution {
                    name: tool_name.clone(),
                    input_preview,
                    output_preview,
                    is_error,
                });
        }

        // Check if tool execution changed the mode (e.g., EnterPlanMode, PresentPlan)
        // and update status bar accordingly
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // Store tool result
        self.tool_results
            .write()
            .await
            .entry(query_id)
            .or_insert_with(Vec::new)
            .push((tool_id, result));

        // Check if all tools for this query have completed
        let metadata = self.query_states.get_metadata(query_id).await;
        if let Some(meta) = metadata {
            if let QueryState::ExecutingTools { tools_pending, .. } = meta.state {
                let results_count = self
                    .tool_results
                    .read()
                    .await
                    .get(&query_id)
                    .map(|v| v.len())
                    .unwrap_or(0);

                if results_count >= tools_pending {
                    // Keep the query-level Tools unit live while the provider
                    // consumes these results. A later tool round appends rows
                    // to the same unit; a final wire program closes it before
                    // opening the distinct program-source unit.
                    self.finalize_tool_execution(query_id).await?;
                }
            }
        }

        Ok(())
    }

    /// Finalize tool execution (all tools complete, re-invoke Claude)
    async fn finalize_tool_execution(&mut self, query_id: Uuid) -> Result<()> {
        // Get all tool results for this query
        let results = self
            .tool_results
            .write()
            .await
            .remove(&query_id)
            .unwrap_or_default();

        // Sync the plan mode status bar.  handle_present_plan() updates the mode Arc
        // but is a free function without &self access, so the indicator update happens here.
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // ── Plan-approval fast path ───────────────────────────────────────────
        // When the user just approved a PresentPlan, the mode is now Executing.
        // The long planning exploration history confuses the model (it forgets the
        // task and re-explores instead of implementing). Reset to a clean context
        // with just the execution directive.
        if matches!(current_mode, ReplMode::Executing { .. }) {
            let plan_directive = results.iter().find_map(|(_, r)| {
                if let Ok(content) = r {
                    if content.starts_with("Plan approved by user.") {
                        Some(content.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            if let Some(directive) = plan_directive {
                // Clear tool-call history so planning-phase reads/globs don't
                // trigger loop detection when Claude calls them again during execution.
                self.tool_call_history.write().await.remove(&query_id);

                // Reset conversation to a single clear execution prompt.
                {
                    let mut conv = self.conversation.write().await;
                    conv.clear();
                    conv.add_user_message(directive);
                }

                let _ = self.llm_tx.send(LlmRequest::Query {
                    id: query_id,
                    text: String::new(),
                    no_tools: false,
                });
                return Ok(());
            }
        }

        // ── Normal path: build ToolResult message and continue ────────────────
        // Create a user message with proper ToolResult content blocks
        let mut content_blocks = Vec::new();
        for (tool_id, result) in results {
            match result {
                Ok(content) => {
                    content_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content,
                        is_error: None,
                    });
                }
                Err(e) => {
                    content_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content: e.to_string(),
                        is_error: Some(true),
                    });
                }
            }
        }

        // Add tool results to conversation as a proper message
        let tool_result_message = crate::claude::Message {
            role: "user".to_string(),
            content: content_blocks,
        };

        self.conversation
            .write()
            .await
            .add_message(tool_result_message);

        // Send tool-continuation turn to the LLM worker loop
        let _ = self.llm_tx.send(LlmRequest::Query {
            id: query_id,
            text: String::new(),
            no_tools: false,
        });

        Ok(())
    }

    /// Handle `/graph` — display the execution graph for the most recent query.
    async fn handle_graph_command(&mut self) -> Result<()> {
        let g = self.current_graph.lock().await;
        if g.is_empty() {
            self.output_manager
                .write_info("No execution graph recorded yet. Run a query first.");
        } else {
            let text = g.format_display();
            // Append save path hint
            let hint = if let Some(qid) = g.query_id {
                let short = &qid.to_string()[..8];
                format!(
                    "\nSaved to ~/.finch/graphs/{}-{}.json",
                    g.session_label, short
                )
            } else {
                String::new()
            };
            self.output_manager.write_info(format!("{}{}", text, hint));
        }
        self.render_tui().await?;
        Ok(())
    }

    /// Add a user-authored task to the reviewable execution plan.
    ///
    /// This operation only edits the plan. It never evaluates the text, asks a
    /// model to invent Forth, or mutates a legacy interpreter dictionary. `/run`
    /// is the separate review and execution boundary.
    async fn handle_stack_push(&mut self, text: String) -> Result<()> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.stack.lock().await.push(text.clone());
        self.poset.lock().await.add_node(
            text,
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );

        self.tui_renderer.lock().await.poset_panel_mode =
            crate::cli::tui::PosetPanelMode::Graph;
        self.render_tui().await
    }

    /// `/chain W1 W2` — add edge W1 → W2 (W2 depends on W1).
    async fn handle_stack_chain(&mut self, a: usize, b: usize) -> Result<()> {
        let ok = {
            let mut p = self.poset.lock().await;
            let has_a = p.nodes.iter().any(|n| n.id == a);
            let has_b = p.nodes.iter().any(|n| n.id == b);
            if has_a && has_b {
                p.edges.push((a, b));
                true
            } else {
                false
            }
        };
        if ok {
            self.output_manager.write_info(format!("W{a} → W{b}"));
        } else {
            self.output_manager
                .write_info(format!("W{a} or W{b} not found"));
        }
        self.render_tui().await
    }

    /// `/forget W1` — remove word and any AI-generated successors.
    async fn handle_stack_forget(&mut self, id: usize) -> Result<()> {
        let removed = {
            let mut p = self.poset.lock().await;
            let mut to_remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
            to_remove.insert(id);
            let mut frontier = vec![id];
            while let Some(cur) = frontier.pop() {
                for &(pred, succ) in &p.edges {
                    if pred == cur && !to_remove.contains(&succ) {
                        if p.nodes.iter().any(|n| {
                            n.id == succ && matches!(n.author, crate::poset::NodeAuthor::Ai)
                        }) {
                            to_remove.insert(succ);
                            frontier.push(succ);
                        }
                    }
                }
            }
            let count = to_remove.len();
            let removed_labels: std::collections::HashSet<String> = p
                .nodes
                .iter()
                .filter(|n| to_remove.contains(&n.id))
                .map(|n| n.label.clone())
                .collect();
            p.nodes.retain(|n| !to_remove.contains(&n.id));
            p.edges
                .retain(|&(a, b)| !to_remove.contains(&a) && !to_remove.contains(&b));
            drop(p);
            let mut s = self.stack.lock().await;
            s.retain(|item| !removed_labels.contains(item));
            count
        };
        self.output_manager.write_info(format!(
            "forgot W{id} ({removed} word{} removed)",
            if removed == 1 { "" } else { "s" }
        ));
        self.render_tui().await
    }

    /// `/dup W1` — clone word W1 as a new entry with no edges.
    async fn handle_stack_dup(&mut self, id: usize) -> Result<()> {
        let result = {
            let mut p = self.poset.lock().await;
            if let Some(node) = p.nodes.iter().find(|n| n.id == id).cloned() {
                let new_id = p.add_node(
                    node.label.clone(),
                    node.kind.clone(),
                    crate::poset::NodeAuthor::User,
                );
                Some((new_id, node.label))
            } else {
                None
            }
        };
        if let Some((new_id, label)) = result {
            self.stack.lock().await.push(label.clone());
            self.output_manager
                .write_info(format!("W{id} → W{new_id}  \"{label}\""));
        } else {
            self.output_manager.write_info(format!("W{id} not found"));
        }
        self.render_tui().await
    }

    /// `/swap W1 W2` — swap the labels of two words.
    async fn handle_stack_swap(&mut self, a: usize, b: usize) -> Result<()> {
        let ok = {
            let mut p = self.poset.lock().await;
            let a_idx = p.nodes.iter().position(|n| n.id == a);
            let b_idx = p.nodes.iter().position(|n| n.id == b);
            if let (Some(ai), Some(bi)) = (a_idx, b_idx) {
                let label_a = p.nodes[ai].label.clone();
                let label_b = p.nodes[bi].label.clone();
                p.nodes[ai].label = label_b;
                p.nodes[bi].label = label_a;
                true
            } else {
                false
            }
        };
        if ok {
            self.output_manager
                .write_info(format!("swapped W{a} ↔ W{b}"));
        } else {
            self.output_manager
                .write_info(format!("W{a} or W{b} not found"));
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
    async fn handle_stack_demo(&mut self) -> Result<()> {
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

    /// Switch the Co-Forth overlay panel to Forth source view.
    /// The overlay recomputes the program from the live poset on each render tick.
    async fn handle_stack_program(&mut self) -> Result<()> {
        let mut tui = self.tui_renderer.lock().await;
        if tui.poset_panel_mode != crate::cli::tui::PosetPanelMode::Forth {
            tui.toggle_poset_view();
        }
        drop(tui);
        self.render_tui().await
    }

    /// Handle `/stack` — show current stack contents.
    async fn handle_stack_show(&mut self) -> Result<()> {
        let stack = self.stack.lock().await;
        if stack.is_empty() {
            self.output_manager
                .write_info("📚 Stack is empty.  Type to push, /pop to execute.");
        } else {
            let mut lines = vec![format!(
                "📚 Stack ({} item{}):",
                stack.len(),
                if stack.len() == 1 { "" } else { "s" }
            )];
            for (i, item) in stack.iter().enumerate() {
                let preview = if item.len() > 80 {
                    format!("{}…", item.chars().take(80).collect::<String>())
                } else {
                    item.clone()
                };
                lines.push(format!("  [{:>2}] {}", i + 1, preview));
            }
            lines.push(String::new());
            lines.push("/pop to execute all as one query.".to_string());
            self.output_manager.write_info(lines.join("\n"));
        }
        drop(stack);
        self.render_tui().await
    }

    /// Handle `/pop` — remove the top item from the stack (undo last push).
    async fn handle_stack_pop(&mut self) -> Result<()> {
        let removed = self.poset.lock().await.pop();
        let Some(removed) = removed else {
            self.output_manager
                .write_info("📚 Plan is empty. Nothing to pop.");
            self.render_tui().await?;
            return Ok(());
        };

        let mut stack = self.stack.lock().await;
        if let Some(index) = stack.iter().rposition(|item| item == &removed.label) {
            stack.remove(index);
        }
        let depth = self.poset.lock().await.nodes.len();
        drop(stack);
        let preview = if removed.label.len() > 60 {
            format!("{}…", removed.label.chars().take(60).collect::<String>())
        } else {
            removed.label
        };
        self.output_manager
            .write_info(format!(
                "📚 removed W{} → \"{preview}\"   nodes:{depth}",
                removed.id
            ));
        self.render_tui().await
    }

    async fn handle_stack_run(&mut self) -> Result<Option<String>> {
        let mut stack = self.stack.lock().await;
        if stack.is_empty() {
            drop(stack);
            self.output_manager
                .write_info("📚 Stack is empty. Type something first.");
            self.render_tui().await?;
            return Ok(None);
        }
        let count = stack.len();
        let query = stack.drain(..).collect::<Vec<_>>().join("\n\n");
        drop(stack);
        self.output_manager.write_info(format!(
            "📚 Running {count} item{}…",
            if count == 1 { "" } else { "s" }
        ));
        self.render_tui().await?;
        Ok(Some(query))
    }

    /// Execute the approved stack: if any poset nodes have tools, run the poset executor;
    /// otherwise fall back to the plain query path.
    async fn handle_poset_or_query(&mut self, query: String) -> Result<()> {
        let is_non_empty = !self.poset.lock().await.is_empty();

        if is_non_empty {
            // Show confirmation dialog (non-blocking); continuation handled by render tick.
            self.confirm_poset_run().await?;
        } else {
            self.execute_query(query).await?;
        }
        Ok(())
    }

    /// Handle `/stack clear` — drop all stack items and return panel to graph view.
    async fn handle_stack_clear(&mut self) -> Result<()> {
        let mut stack = self.stack.lock().await;
        let count = self.poset.lock().await.nodes.len();
        stack.clear();
        drop(stack);
        self.poset.lock().await.clear();
        // Return panel to graph view so the user is back in normal chat mode.
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.poset_panel_mode = crate::cli::tui::PosetPanelMode::Graph;
        }
        if count == 0 {
            self.output_manager
                .write_info("stack empty  (tip: ?? question  to ask the AI directly)");
        } else {
            self.output_manager.write_info(format!(
                "cleared {count} item{}  (tip: ?? question  to ask the AI directly)",
                if count == 1 { "" } else { "s" }
            ));
        }
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
    async fn confirm_poset_run(&mut self) -> Result<()> {
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

    /// Handle tool approval request (show dialog, get user response)
    async fn handle_tool_approval_request(
        &mut self,
        query_id: Uuid,
        tool_use: crate::tools::types::ToolUse,
        response_tx: tokio::sync::oneshot::Sender<super::events::ConfirmationResult>,
    ) -> Result<()> {
        use crate::cli::tui::Dialog;

        tracing::debug!("[EVENT_LOOP] Requesting tool approval: {}", tool_use.name);

        let approval_audience = self
            .pending_named_brain_turns
            .get(&query_id)
            .map(|turn| turn.approval_audience.clone());
        let approval_tx = self
            .pending_named_brain_turns
            .get(&query_id)
            .and_then(|turn| turn.approval_tx.clone());
        if let (Some(approval_tx), Some(audience)) =
            (approval_tx, approval_audience.as_ref())
        {
            let event = crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: tool_use.id.clone(),
                approval_kind: "tool".to_string(),
                subject: tool_use.name.clone(),
                audience: audience.clone(),
                detail: serde_json::json!({"input": tool_use.input.clone()}),
            };
            let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
            if approval_tx
                .send(crate::server::RunnerApprovalRequest {
                    event,
                    response_tx: decision_tx,
                })
                .is_err()
            {
                let _ = response_tx.send(super::events::ConfirmationResult::Deny);
                return Ok(());
            }
            tokio::spawn(async move {
                let confirmation = decision_rx
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .and_then(|decision| {
                        confirmation_from_audit_value(&decision, &tool_use).ok()
                    })
                    .unwrap_or(super::events::ConfirmationResult::Deny);
                let _ = response_tx.send(confirmation);
            });
            return Ok(());
        }
        if let (Some(turn), Some(audience)) = (
            self.pending_named_brain_turns.get_mut(&query_id),
            approval_audience.as_ref(),
        ) {
            turn.turn_events
                .push(crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: tool_use.id.clone(),
                    approval_kind: "tool".to_string(),
                    subject: tool_use.name.clone(),
                    audience: audience.clone(),
                    detail: serde_json::json!({"input": tool_use.input.clone()}),
                });
        }

        // Create approval dialog — compact 3-option style matching Claude Code UX
        let tool_name = &tool_use.name;
        let mut summary = tool_approval_summary(&tool_use);
        if let Some(audience) = approval_audience {
            summary.push_str("\n\n");
            summary.push_str(&approval_audience_summary(&audience));
        }

        let dialog = Dialog::tool_approval(tool_name, &summary);

        // Set dialog in TUI (non-blocking - will be handled by async_input task)
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);

        // Force render to show dialog immediately
        if let Err(e) = tui.render() {
            tracing::error!("[EVENT_LOOP] Failed to render dialog: {}", e);
        }
        drop(tui);

        // Store the response channel and tool_use for when dialog completes
        // We'll check pending_dialog_result in the event loop and send the response then
        self.pending_approvals
            .write()
            .await
            .insert(query_id, (tool_use, response_tx));

        tracing::debug!("[EVENT_LOOP] Tool approval dialog shown, waiting for user response");

        Ok(())
    }

    /// Present one exact typed-VM capability request. Unlike legacy tool
    /// approval, these choices become structured grant scopes and are checked
    /// again against the retained ProgramRun before any authority is issued.
    async fn handle_vm_approval_request(
        &mut self,
        prompt: crate::vm::ApprovalPrompt,
        response_tx: tokio::sync::oneshot::Sender<crate::vm::ApprovalChoice>,
    ) -> Result<()> {
        if self.pending_vm_approval.is_some() {
            let _ = response_tx.send(crate::vm::ApprovalChoice::Deny);
            self.output_manager.write_error(
                "Denied a concurrent VM capability request while another approval dialog was active",
            );
            return Ok(());
        }

        let query_id = *self.active_query_id.read().await;
        let approval_id = prompt.request.id.to_string();
        let approval_tx = query_id.and_then(|query_id| {
            self.pending_named_brain_turns
                .get(&query_id)
                .and_then(|turn| turn.approval_tx.clone())
        });
        if let (Some(query_id), Some(approval_tx)) = (query_id, approval_tx) {
            let audience = self
                .pending_named_brain_turns
                .get(&query_id)
                .expect("named Brain turn disappeared while requesting approval")
                .approval_audience
                .clone();
            let event = crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: approval_id.clone(),
                approval_kind: "vm_capability".to_string(),
                subject: format!("{:?}", prompt.exact.capability),
                audience,
                detail: serde_json::to_value(&prompt)
                    .unwrap_or_else(|_| serde_json::json!({"reason": prompt.request.reason.clone()})),
            };
            let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
            if approval_tx
                .send(crate::server::RunnerApprovalRequest {
                    event,
                    response_tx: decision_tx,
                })
                .is_err()
            {
                let _ = response_tx.send(crate::vm::ApprovalChoice::Deny);
                return Ok(());
            }
            tokio::spawn(async move {
                let choice = decision_rx
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .and_then(|decision| serde_json::from_value(decision).ok())
                    .unwrap_or(crate::vm::ApprovalChoice::Deny);
                let _ = response_tx.send(choice);
            });
            return Ok(());
        }
        if let Some(query_id) = query_id {
            if let Some(turn) = self.pending_named_brain_turns.get_mut(&query_id) {
                turn.turn_events
                    .push(crate::server::RunnerTurnEvent::ApprovalRequested {
                        approval_id: approval_id.clone(),
                        approval_kind: "vm_capability".to_string(),
                        subject: format!("{:?}", prompt.exact.capability),
                        audience: turn.approval_audience.clone(),
                        detail: serde_json::to_value(&prompt).unwrap_or_else(|_| {
                            serde_json::json!({"reason": prompt.request.reason.clone()})
                        }),
                    });
            }
        }

        let choices = vm_approval_choices(&prompt);
        let audience = query_id
            .and_then(|query_id| self.pending_named_brain_turns.get(&query_id))
            .map(|turn| &turn.approval_audience);
        let dialog = vm_approval_dialog(&prompt, audience, self.program_runtime.as_ref());

        self.pending_vm_approval = Some(PendingVmApproval {
            response_tx,
            choices,
            query_id,
            approval_id,
        });
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);
        tui.pending_dialog_result = None;
        tui.render()?;
        Ok(())
    }

    /// Convert dialog result to confirmation result
    fn dialog_result_to_confirmation(
        &self,
        dialog_result: crate::cli::tui::DialogResult,
        tool_use: &crate::tools::types::ToolUse,
    ) -> super::events::ConfirmationResult {
        dialog_result_to_confirmation(dialog_result, tool_use)
    }

    // ========== Plan Mode Handlers ==========

    /// Update status bar with current plan mode indicator
    fn update_plan_mode_indicator(&self, mode: &ReplMode) {
        use crate::cli::status_bar::StatusLineType;

        let indicator = match mode {
            ReplMode::Normal => "⏵⏵ accept edits on (shift+tab to cycle)",
            ReplMode::Planning { .. } => "⏸ plan mode on (shift+tab to cycle)",
            ReplMode::Executing { .. } => "▶ executing plan (shift+tab disabled)",
        };

        self.status_bar
            .update_line(StatusLineType::Custom("plan_mode".to_string()), indicator);
    }

    #[allow(dead_code)]
    /// Handle /plan command - enter planning mode
    async fn handle_plan_command(&mut self, task: String) -> Result<()> {
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
        self.output_manager.write_info("  bash, save_and_exec");
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
             Blocked tools: bash, save_and_exec\n\
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
    async fn handle_plan_task(&mut self, task: String) -> Result<()> {
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

include!("brain_handler.rs");

// handle_present_plan, handle_ask_user_question, is_tool_allowed_in_mode moved to plan_handler.rs

/// Find the most recent (query, response) pair from conversation history.
///
/// Scans messages in reverse: finds the latest non-empty assistant message,
/// then finds the user message that immediately preceded it.
///
/// Returns `("", "")` if no assistant response is found.
pub(crate) fn find_last_exchange(messages: &[crate::claude::Message]) -> (String, String) {
    let mut last_response = String::new();
    let mut last_query = String::new();
    let mut found_response = false;

    for msg in messages.iter().rev() {
        if !found_response && msg.role == "assistant" {
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        last_response = text.clone();
                        found_response = true;
                        break;
                    }
                }
            }
        } else if found_response && msg.role == "user" {
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        last_query = text.clone();
                        break;
                    }
                }
            }
            break;
        }
    }

    (last_query, last_response)
}

/// Build a concise human-readable summary of a tool call for the approval dialog.
///
/// Returns a single line such as `"Command: git push"` or `"File: src/main.rs"`.
/// Exported `pub(crate)` so it can be unit-tested directly.
pub(crate) fn tool_approval_summary(tool_use: &crate::tools::types::ToolUse) -> String {
    let tool_name = &tool_use.name;
    match tool_name.as_str() {
        "bash" | "Bash" => {
            if let Some(cmd) = tool_use.input.get("command").and_then(|v| v.as_str()) {
                format!(
                    "Command: {}",
                    if cmd.len() > 60 {
                        format!("{}...", cmd.chars().take(60).collect::<String>())
                    } else {
                        cmd.to_string()
                    }
                )
            } else {
                "Execute shell command".to_string()
            }
        }
        "read" | "Read" => {
            if let Some(path) = tool_use.input.get("file_path").and_then(|v| v.as_str()) {
                format!("File: {}", path)
            } else {
                "Read file".to_string()
            }
        }
        "grep" | "Grep" => {
            if let Some(pattern) = tool_use.input.get("pattern").and_then(|v| v.as_str()) {
                format!(
                    "Pattern: {}",
                    if pattern.len() > 40 {
                        format!("{}...", pattern.chars().take(40).collect::<String>())
                    } else {
                        pattern.to_string()
                    }
                )
            } else {
                "Search files".to_string()
            }
        }
        "glob" | "Glob" => {
            if let Some(pattern) = tool_use.input.get("pattern").and_then(|v| v.as_str()) {
                format!("Pattern: {}", pattern)
            } else {
                "Find files".to_string()
            }
        }
        "enter_plan_mode" | "EnterPlanMode" => {
            if let Some(reason) = tool_use.input.get("reason").and_then(|v| v.as_str()) {
                format!(
                    "Reason: {}",
                    if reason.len() > 50 {
                        format!("{}...", reason.chars().take(50).collect::<String>())
                    } else {
                        reason.to_string()
                    }
                )
            } else {
                "Enter planning mode".to_string()
            }
        }
        "write" | "Write" => {
            let path = tool_use
                .input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let content = tool_use
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let line_count = content.lines().count();
            let is_new = !std::path::Path::new(path).exists();
            if is_new {
                let preview: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
                let truncated = if line_count > 5 {
                    format!("\n… ({} lines total)", line_count)
                } else {
                    String::new()
                };
                format!("Create {}\n{}{}", path, preview, truncated)
            } else {
                // Show unified diff against existing file
                let existing = std::fs::read_to_string(path).unwrap_or_default();
                format!(
                    "Overwrite {}\n{}",
                    path,
                    unified_diff_summary(&existing, content, 3)
                )
            }
        }
        "edit" | "Edit" => {
            let path = tool_use
                .input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let old = tool_use
                .input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = tool_use
                .input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Edit {}\n{}", path, unified_diff_summary(old, new, 2))
        }
        _ => format!("Execute {} tool", tool_name),
    }
}

/// Produce a compact unified-diff-style summary between `before` and `after`.
/// Shows up to `context` lines of context around each change.
fn unified_diff_summary(before: &str, after: &str, context: usize) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Simple LCS-based diff: find changed regions
    let mut hunks: Vec<String> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut current_hunk: Vec<String> = Vec::new();
    let mut in_hunk = false;
    let max_lines = 40; // cap total output
    let mut total = 0;

    // Build a simple line-by-line diff without external crates:
    // walk both sides, emit - / + lines for mismatches
    while i < before_lines.len() || j < after_lines.len() {
        if total >= max_lines {
            current_hunk.push(format!("  … (diff truncated)"));
            break;
        }
        match (before_lines.get(i), after_lines.get(j)) {
            (Some(a), Some(b)) if a == b => {
                if in_hunk {
                    current_hunk.push(format!("  {}", a));
                    // End hunk after `context` unchanged lines
                    let trail = current_hunk
                        .iter()
                        .rev()
                        .take_while(|l| l.starts_with("  "))
                        .count();
                    if trail > context {
                        hunks.push(current_hunk.join("\n"));
                        current_hunk = Vec::new();
                        in_hunk = false;
                    }
                }
                i += 1;
                j += 1;
            }
            (Some(a), Some(_b)) => {
                if !in_hunk {
                    // Add context before
                    let start = i.saturating_sub(context);
                    for ctx_line in before_lines[start..i].iter() {
                        current_hunk.push(format!("  {}", ctx_line));
                    }
                    in_hunk = true;
                }
                current_hunk.push(format!("- {}", a));
                current_hunk.push(format!("+ {}", _b));
                total += 2;
                i += 1;
                j += 1;
            }
            (Some(a), None) => {
                if !in_hunk {
                    in_hunk = true;
                }
                current_hunk.push(format!("- {}", a));
                total += 1;
                i += 1;
            }
            (None, Some(b)) => {
                if !in_hunk {
                    in_hunk = true;
                }
                current_hunk.push(format!("+ {}", b));
                total += 1;
                j += 1;
            }
            (None, None) => break,
        }
    }
    if !current_hunk.is_empty() {
        hunks.push(current_hunk.join("\n"));
    }
    if hunks.is_empty() {
        "(no changes)".to_string()
    } else {
        hunks.join("\n---\n")
    }
}

/// Convert a dialog selection to a `ConfirmationResult` for tool approval.
///
/// 3-option mapping (Claude Code style):
///   - `Selected(0)` → `ApproveOnce`            ("1. Yes")
///   - `Selected(1)` → `ApprovePatternSession`   ("2. Yes, and don't ask again for: tool:*")
///   - `Selected(2+)` / `Cancelled` → `Deny`     ("3. No")
///
/// Exported `pub(crate)` so it can be unit-tested directly.
pub(crate) fn dialog_result_to_confirmation(
    dialog_result: crate::cli::tui::DialogResult,
    tool_use: &crate::tools::types::ToolUse,
) -> super::events::ConfirmationResult {
    use super::events::ConfirmationResult;
    use crate::tools::patterns::ToolPattern;

    match dialog_result {
        crate::cli::tui::DialogResult::Selected(index) => match index {
            0 => ConfirmationResult::ApproveOnce,
            1 => {
                // Session-wide wildcard: don't ask again for any call to this tool.
                let pattern = ToolPattern::new(
                    "*".to_string(),
                    tool_use.name.clone(),
                    format!("Allow all {} calls (session)", tool_use.name),
                );
                ConfirmationResult::ApprovePatternSession(pattern)
            }
            _ => ConfirmationResult::Deny, // "3. No" or anything beyond
        },
        _ => ConfirmationResult::Deny,
    }
}

fn confirmation_audit_value(
    confirmation: &super::events::ConfirmationResult,
) -> serde_json::Value {
    use super::events::ConfirmationResult;

    match confirmation {
        ConfirmationResult::ApproveOnce => serde_json::json!({"choice": "approve_once"}),
        ConfirmationResult::ApproveExactSession(signature) => serde_json::json!({
            "choice": "approve_exact_session",
            "tool": signature.tool_name,
            "context_key": signature.context_key,
            "command": signature.command,
            "args": signature.args,
            "directory": signature.directory,
        }),
        ConfirmationResult::ApprovePatternSession(pattern) => serde_json::json!({
            "choice": "approve_pattern_session",
            "pattern_id": pattern.id,
            "pattern": pattern.pattern,
            "tool": pattern.tool_name,
        }),
        ConfirmationResult::ApproveExactPersistent(signature) => serde_json::json!({
            "choice": "approve_exact_persistent",
            "tool": signature.tool_name,
            "context_key": signature.context_key,
            "command": signature.command,
            "args": signature.args,
            "directory": signature.directory,
        }),
        ConfirmationResult::ApprovePatternPersistent(pattern) => serde_json::json!({
            "choice": "approve_pattern_persistent",
            "pattern_id": pattern.id,
            "pattern": pattern.pattern,
            "tool": pattern.tool_name,
        }),
        ConfirmationResult::ApproveWithInput(input) => serde_json::json!({
            "choice": "approve_with_edited_input",
            "input": input,
        }),
        ConfirmationResult::Deny => serde_json::json!({"choice": "deny"}),
    }
}

fn confirmation_from_audit_value(
    decision: &serde_json::Value,
    tool_use: &crate::tools::types::ToolUse,
) -> anyhow::Result<super::events::ConfirmationResult> {
    use super::events::ConfirmationResult;

    match decision.get("choice").and_then(serde_json::Value::as_str) {
        Some("approve_once") => Ok(ConfirmationResult::ApproveOnce),
        Some("approve_pattern_session") => Ok(ConfirmationResult::ApprovePatternSession(
            crate::tools::patterns::ToolPattern::new(
                "*".to_string(),
                tool_use.name.clone(),
                format!("Allow all {} calls (session)", tool_use.name),
            ),
        )),
        Some("approve_with_edited_input") => Ok(ConfirmationResult::ApproveWithInput(
            decision
                .get("input")
                .cloned()
                .context("edited approval decision omitted its input")?,
        )),
        Some("deny") => Ok(ConfirmationResult::Deny),
        Some(choice) => anyhow::bail!("unsupported remote tool approval choice '{choice}'"),
        None => anyhow::bail!("remote tool approval decision omitted its choice"),
    }
}

// ── Unified diff applicator ───────────────────────────────────────────────────
//
// A line-based applicator for the unified diff format produced by `diff -u`.
// Handles context, additions, and deletions.  Does not handle "no newline at
// end of file" markers (`\ No newline at end of file`) — they are ignored.

fn apply_patch_lines(original: &[String], patch: &str) -> anyhow::Result<Vec<String>> {
    let mut result: Vec<String> = Vec::new();
    let mut orig_pos: usize = 0; // 0-based index into `original`

    let mut in_hunk = false;
    // Per-hunk state
    let mut hunk_orig_start: usize = 0;
    let mut hunk_orig_len: usize = 0;
    let mut hunk_orig_consumed: usize = 0;

    for line in patch.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            // File header — skip
            continue;
        }
        if line.starts_with("@@ ") {
            // Parse: @@ -l,s +l,s @@
            // e.g.  @@ -3,7 +3,6 @@
            if in_hunk {
                // Flush the rest of the previous hunk
                while hunk_orig_consumed < hunk_orig_len {
                    let idx = hunk_orig_start + hunk_orig_consumed;
                    if idx < original.len() {
                        result.push(original[idx].clone());
                    }
                    hunk_orig_consumed += 1;
                }
            }
            // Parse the @@ line
            let (os, ol) = parse_hunk_header(line)?;
            // Copy unmodified lines from current position up to this hunk's start
            let hunk_start_0 = os.saturating_sub(1); // convert 1-based to 0-based
            while orig_pos < hunk_start_0 && orig_pos < original.len() {
                result.push(original[orig_pos].clone());
                orig_pos += 1;
            }
            hunk_orig_start = hunk_start_0;
            hunk_orig_len = ol;
            hunk_orig_consumed = 0;
            in_hunk = true;
            orig_pos = hunk_start_0;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if line.starts_with('\\') {
            // "\ No newline at end of file" — ignore
            continue;
        }
        if let Some(content) = line.strip_prefix(' ') {
            // Context line — keep it
            result.push(content.to_string());
            hunk_orig_consumed += 1;
            orig_pos += 1;
        } else if let Some(content) = line.strip_prefix('+') {
            // Addition — insert it
            result.push(content.to_string());
        } else if line.starts_with('-') {
            // Removal — skip the original line
            hunk_orig_consumed += 1;
            orig_pos += 1;
        }
    }

    // After all hunks: flush remaining original lines
    while orig_pos < original.len() {
        result.push(original[orig_pos].clone());
        orig_pos += 1;
    }

    Ok(result)
}

/// Parse the `-l,s` part of `@@ -l,s +l,s @@`.
/// Returns `(start, len)` for the original (minus) side.
fn parse_hunk_header(line: &str) -> anyhow::Result<(usize, usize)> {
    // Format: @@ -<start>[,<len>] +<start>[,<len>] @@
    let rest = line
        .strip_prefix("@@ ")
        .ok_or_else(|| anyhow::anyhow!("bad hunk header: {line}"))?;
    let minus = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no minus range in hunk header"))?;
    let minus = minus
        .strip_prefix('-')
        .ok_or_else(|| anyhow::anyhow!("minus range doesn't start with '-': {minus}"))?;
    let (start_str, len_str) = minus.split_once(',').unwrap_or((minus, "1"));
    let start: usize = start_str
        .parse()
        .map_err(|_| anyhow::anyhow!("bad start in hunk: {start_str}"))?;
    let len: usize = len_str
        .parse()
        .map_err(|_| anyhow::anyhow!("bad len in hunk: {len_str}"))?;
    Ok((start, len))
}

#[cfg(test)]
mod tests {
    #[test]
    fn approval_audit_value_preserves_the_decision_scope() {
        let decision = super::confirmation_audit_value(
            &super::super::events::ConfirmationResult::ApproveOnce,
        );
        assert_eq!(decision, serde_json::json!({"choice": "approve_once"}));
    }

    #[test]
    fn remote_tool_approval_round_trips_edited_input() {
        let tool_use = crate::tools::types::ToolUse {
            id: "tool-1".into(),
            name: "edit".into(),
            input: serde_json::json!({"path": "src/main.rs", "new_string": "old"}),
        };
        let edited = serde_json::json!({"path": "src/main.rs", "new_string": "new"});
        let audit = super::confirmation_audit_value(
            &super::super::events::ConfirmationResult::ApproveWithInput(edited.clone()),
        );

        assert!(matches!(
            super::confirmation_from_audit_value(&audit, &tool_use).unwrap(),
            super::super::events::ConfirmationResult::ApproveWithInput(input)
                if input == edited
        ));
    }

    use super::*;

    fn brain_event(
        seq: u64,
        sender: &str,
        kind: crate::brain::shared::BrainEventKind,
    ) -> crate::brain::shared::BrainEvent {
        crate::brain::shared::BrainEvent {
            schema_version: 1,
            brain_id: crate::brain::shared::BrainId(uuid::Uuid::nil()),
            seq,
            environment_generation: 1,
            sender: sender.into(),
            created_ms: 0,
            kind,
        }
    }

    #[test]
    fn local_runner_projection_suppresses_matching_canonical_program_and_result() {
        let mut projection = LocalBrainProjection {
            source: "(say \"hello\")".into(),
            output: "hello".into(),
            tool_ids: std::collections::HashSet::new(),
            approval_ids: std::collections::HashSet::new(),
            program_seq: None,
        };
        let program = brain_event(
            12,
            "provider",
            crate::brain::shared::BrainEventKind::Program {
                language: crate::brain::shared::ProgramLanguage::Lisp,
                source: "(say \"hello\")".into(),
            },
        );
        assert_eq!(
            projection.observe(&program),
            LocalProjectionMatch::Suppress
        );
        assert_eq!(projection.program_seq, Some(12));

        let result = brain_event(
            14,
            "daemon",
            crate::brain::shared::BrainEventKind::Result {
                request_seq: 12,
                output: "hello".into(),
                error: None,
            },
        );
        assert_eq!(
            projection.observe(&result),
            LocalProjectionMatch::SuppressAndComplete
        );
    }

    #[test]
    fn local_runner_projection_does_not_hide_different_canonical_output() {
        let mut projection = LocalBrainProjection {
            source: "(say \"hello\")".into(),
            output: "hello".into(),
            tool_ids: std::collections::HashSet::new(),
            approval_ids: std::collections::HashSet::new(),
            program_seq: Some(12),
        };
        let result = brain_event(
            14,
            "daemon",
            crate::brain::shared::BrainEventKind::Result {
                request_seq: 12,
                output: "different".into(),
                error: None,
            },
        );
        assert_eq!(projection.observe(&result), LocalProjectionMatch::None);
    }

    #[test]
    fn participant_subject_is_not_the_brain_name() {
        assert_eq!(
            participant_subject_from("shammah", "workstation.local"),
            "shammah@workstation.local"
        );
    }

    #[test]
    fn participant_subject_is_printable_and_bounded() {
        let subject = participant_subject_from(&format!("user\n{}", "x".repeat(200)), "");
        assert!(!subject.chars().any(char::is_control));
        assert!(subject.chars().count() <= 128);
        assert!(subject.ends_with('x'));
    }

    #[test]
    fn runner_subject_identifies_one_frontend_not_only_its_participant() {
        let participant = "shammah@workstation.local";
        let first = runner_subject_from(
            participant,
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        );
        let second = runner_subject_from(
            participant,
            Uuid::parse_str("11111111-0000-0000-0000-000000000001").unwrap(),
        );
        assert_ne!(first, second);
        assert!(first.starts_with(participant));
        assert!(first.chars().count() <= 128);
    }
    use crate::cli::repl_event::query_processor::apply_sliding_window;
    // format_elapsed and format_token_count moved to tool_display; import for status-bar tests.
    use crate::cli::repl_event::tool_display::{format_elapsed, format_token_count};

    // Pulsing animation frames used in status-bar tests.
    const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

    fn claude_profile(name: &str, model: &str) -> crate::config::ProviderEntry {
        crate::config::ProviderEntry::Claude {
            api_key: "test-key".to_string(),
            model: Some(model.to_string()),
            base_url: None,
            name: Some(name.to_string()),
        }
    }

    #[test]
    fn test_model_profile_resolution_distinguishes_same_provider_models() {
        let profiles = vec![
            claude_profile("fast", "claude-haiku"),
            claude_profile("deep", "claude-opus"),
        ];
        assert_eq!(resolve_provider_profile(&profiles, "fast"), Ok(0));
        assert_eq!(resolve_provider_profile(&profiles, "deep"), Ok(1));
        assert_eq!(resolve_provider_profile(&profiles, "2"), Ok(1));
        assert!(resolve_provider_profile(&profiles, "claude")
            .unwrap_err()
            .contains("multiple profiles"));
    }

    #[test]
    fn test_duplicate_model_profile_names_are_rejected_as_ambiguous() {
        let profiles = vec![
            claude_profile("work", "claude-haiku"),
            claude_profile("work", "claude-opus"),
        ];
        assert!(resolve_provider_profile(&profiles, "work")
            .unwrap_err()
            .contains("ambiguous"));
    }

    // --- streaming status bar format ---

    #[test]
    fn test_streaming_status_format() {
        // Verify the status bar message format used during streaming
        let verb = "Thinking"; // representative word; actual value comes from random_spinner_verb()
        let secs = 75u64;
        let tokens = 1600usize;
        let elapsed_str = format_elapsed(secs);
        let tokens_str = format_token_count(tokens);
        let icon = THROB_FRAMES[1]; // "✳"
        let status = format!(
            "{} {}… ({} · ↓ {} tokens)",
            icon, verb, elapsed_str, tokens_str
        );
        assert_eq!(status, "✳ Thinking… (1m 15s · ↓ 1.6k tokens)");
    }

    #[test]
    fn test_streaming_status_format_short() {
        let verb = "Thinking";
        let secs = 9u64;
        let tokens = 42usize;
        let icon = THROB_FRAMES[0]; // "✦"
        let status = format!(
            "{} {}… ({} · ↓ {} tokens)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(tokens)
        );
        assert_eq!(status, "✦ Thinking… (9s · ↓ 42 tokens)");
    }

    #[test]
    fn test_streaming_status_thinking() {
        // While thinking (no text yet), status shows "· thinking" suffix
        let verb = "Thinking";
        let secs = 15u64;
        let icon = THROB_FRAMES[2]; // "✼"
        let status = format!("{} {}… ({} · thinking)", icon, verb, format_elapsed(secs));
        assert_eq!(status, "✼ Thinking… (15s · thinking)");
    }

    #[test]
    fn test_streaming_status_with_input_tokens() {
        // With input token count available, show ↑ input · ↓ output
        let verb = "Thinking";
        let input_tokens: u32 = 1250;
        let output_tokens = 300usize;
        let secs = 10u64;
        let icon = THROB_FRAMES[1]; // "✳"
        let status = format!(
            "{} {}… ({} · ↑ {} · ↓ {} tokens)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(input_tokens as usize),
            format_token_count(output_tokens),
        );
        assert_eq!(status, "✳ Thinking… (10s · ↑ 1.2k · ↓ 300 tokens)");
    }

    #[test]
    fn test_streaming_status_thinking_with_input_tokens() {
        // Usage arrives before text — show ↑ input · thinking
        let verb = "Thinking";
        let input_tokens: u32 = 800;
        let secs = 3u64;
        let icon = THROB_FRAMES[0]; // "✦"
        let status = format!(
            "{} {}… ({} · ↑ {} · thinking)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(input_tokens as usize),
        );
        assert_eq!(status, "✦ Thinking… (3s · ↑ 800 · thinking)");
    }

    #[test]
    fn test_throb_frames_cycle() {
        // Frames cycle without panicking
        let mut idx = 0usize;
        for _ in 0..100 {
            idx = (idx + 1) % THROB_FRAMES.len();
            assert!(!THROB_FRAMES[idx].is_empty());
        }
        // After 4 steps we're back to frame 0
        assert_eq!(THROB_FRAMES.len(), 4);
    }

    // compact_tool_summary, tool_result_to_display, strip_ansi, bash_smart_summary
    // tests moved to tool_display.rs (where those functions now live).

    // ── PresentPlan display ───────────────────────────────────────────────────

    #[test]
    fn test_presentplan_label_shows_plan_title() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "PresentPlan",
            &serde_json::json!({"plan": "# Refactor Auth System\n\nDetails here..."}),
        );
        assert!(
            label.contains("Refactor Auth System"),
            "label should show plan title: {:?}",
            label
        );
        assert!(
            label.contains("PresentPlan"),
            "label should show tool name: {:?}",
            label
        );
    }

    #[test]
    fn test_presentplan_label_fallback_when_no_heading() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "PresentPlan",
            &serde_json::json!({"plan": "Just some prose with no heading."}),
        );
        assert!(
            label.contains("proposing plan"),
            "should fall back to 'proposing plan': {:?}",
            label
        );
    }

    #[test]
    fn test_presentplan_label_uses_first_heading_only() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "presentplan",
            &serde_json::json!({"plan": "# First Title\n## Second Title\n\nContent"}),
        );
        assert!(
            label.contains("First Title"),
            "should use first heading: {:?}",
            label
        );
        assert!(
            !label.contains("Second Title"),
            "should not show second heading: {:?}",
            label
        );
    }

    // --- find_last_exchange ---

    fn user_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn assistant_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn find_last_exchange_empty_returns_empty_pair() {
        let (q, r) = find_last_exchange(&[]);
        assert!(q.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn find_last_exchange_only_user_messages() {
        let msgs = vec![user_msg("hello"), user_msg("world")];
        let (q, r) = find_last_exchange(&msgs);
        assert!(
            r.is_empty(),
            "no assistant msg → response should be empty: {:?}",
            r
        );
        assert!(q.is_empty());
    }

    #[test]
    fn find_last_exchange_single_turn() {
        let msgs = vec![user_msg("What is 2+2?"), assistant_msg("4")];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(q, "What is 2+2?");
        assert_eq!(r, "4");
    }

    #[test]
    fn find_last_exchange_picks_latest_turn() {
        let msgs = vec![
            user_msg("First question"),
            assistant_msg("First answer"),
            user_msg("Second question"),
            assistant_msg("Second answer"),
        ];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(q, "Second question");
        assert_eq!(r, "Second answer");
    }

    #[test]
    fn find_last_exchange_skips_empty_assistant_text() {
        let msgs = vec![
            user_msg("Real question"),
            assistant_msg("Real answer"),
            user_msg("Ignored"),
            // Assistant message with empty text (e.g., tool-only response)
            crate::claude::Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "   ".to_string(),
                }],
            },
        ];
        let (q, r) = find_last_exchange(&msgs);
        // Should skip the whitespace-only assistant msg and find the earlier real one
        assert_eq!(r, "Real answer");
        assert_eq!(q, "Real question");
    }

    #[test]
    fn find_last_exchange_assistant_only_no_preceding_user() {
        let msgs = vec![assistant_msg("Unprompted response")];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(r, "Unprompted response");
        // No user message precedes it
        assert!(q.is_empty(), "query should be empty: {:?}", q);
    }

    // --- apply_sliding_window ---

    fn make_msgs(roles: &[&str]) -> Vec<crate::claude::Message> {
        roles
            .iter()
            .enumerate()
            .map(|(i, &role)| {
                let text = format!("msg {}", i);
                if role == "user" {
                    user_msg(&text)
                } else {
                    assistant_msg(&text)
                }
            })
            .collect()
    }

    #[test]
    fn test_sliding_window_trims_to_max_verbatim() {
        // 30 alternating messages, max 20 → 20 returned, first is user
        let roles: Vec<&str> = (0..30)
            .map(|i| if i % 2 == 0 { "user" } else { "assistant" })
            .collect();
        let msgs = make_msgs(&roles);
        let result = apply_sliding_window(msgs, 20);
        assert_eq!(result.len(), 20);
        assert_eq!(result.first().unwrap().role, "user");
    }

    #[test]
    fn test_sliding_window_disabled_when_zero() {
        let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
        let len = msgs.len();
        let result = apply_sliding_window(msgs, 0);
        assert_eq!(result.len(), len);
    }

    #[test]
    fn test_sliding_window_no_op_when_under_limit() {
        let msgs = make_msgs(&["user", "assistant", "user", "assistant"]);
        let result = apply_sliding_window(msgs, 20);
        assert_eq!(result.len(), 4);
        assert_eq!(result.first().unwrap().role, "user");
    }

    #[test]
    fn test_sliding_window_skips_orphaned_assistant_at_boundary() {
        // 5 messages: u a u a u, window=3 → last 3 are [a, u, a] (index 2,3,4)
        // Leading 'a' gets skipped → result is [u, a] starting at index 3
        let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
        // Swap last 3 to [assistant, user, assistant] by building manually:
        let roles = ["user", "assistant", "user", "assistant", "user"];
        // With window=3: last 3 = msgs[2..] = [user, assistant, user] → starts with user already
        // To actually trigger the skip, build a window that starts with assistant:
        let msgs2 = make_msgs(&["user", "assistant", "assistant", "user", "assistant"]);
        // window=3 → last 3 = [assistant(idx2), user(idx3), assistant(idx4)]
        // leading assistant removed → [user, assistant]
        let result = apply_sliding_window(msgs2, 3);
        assert_eq!(result.first().unwrap().role, "user");
        assert!(result.len() < 3); // shortened due to skipping
        let _ = roles; // silence unused warning
        let _ = msgs;
    }

    #[test]
    fn test_sliding_window_minimum_guard_prevents_empty() {
        // All messages are assistant-role (pathological case)
        let msgs = make_msgs(&["assistant", "assistant", "assistant", "assistant"]);
        // window=3 → last 3 are all assistant; floor at 2 prevents empty
        let result = apply_sliding_window(msgs, 3);
        assert!(
            result.len() >= 2,
            "floor of 2 must be maintained; got {}",
            result.len()
        );
    }

    /// Regression: orphaned tool_result at window boundary must be stripped.
    ///
    /// Scenario: conversation has two full tool-call round-trips followed by a
    /// user text turn.  With a small window, the first round-trip's tool_use is
    /// cut but its tool_result survives as the first message in the window.
    /// All providers reject `tool_result` blocks without a matching `tool_use`.
    #[test]
    fn test_sliding_window_strips_orphaned_tool_result_at_boundary() {
        use crate::claude::Message;

        // Build:
        //   [0] user "question"          ← will be dropped by window
        //   [1] assistant with ToolUse   ← will be dropped by window (cut here)
        //   [2] user with ToolResult     ← ORPHANED — tool_use was dropped
        //   [3] assistant "answer 1"
        //   [4] user "next question"
        //   [5] assistant "answer 2"
        let tool_use_id = "call_orphan_test".to_string();

        let msgs: Vec<Message> = vec![
            // [0] old user turn (outside window)
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "question".to_string(),
                }],
            },
            // [1] assistant with ToolUse (will be cut by window)
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: tool_use_id.clone(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            },
            // [2] user with ToolResult — orphaned when [1] is cut
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: "file1.rs\nfile2.rs".to_string(),
                    is_error: None,
                }],
            },
            // [3] assistant reply
            assistant_msg("answer 1"),
            // [4] next user turn
            user_msg("next question"),
            // [5] assistant reply
            assistant_msg("answer 2"),
        ];

        // window=4 keeps msgs[2..] = [orphaned ToolResult user, assistant, user, assistant]
        let result = apply_sliding_window(msgs, 4);

        // The orphaned tool_result user turn ([2]) and its assistant response ([3])
        // must have been stripped, leaving [user "next question", assistant "answer 2"].
        assert!(
            result.len() >= 2,
            "must have at least 2 messages; got {}",
            result.len()
        );
        assert_eq!(
            result.first().unwrap().role,
            "user",
            "window must start with a user message"
        );
        // Crucially: the first user message must NOT be a tool_result-only message.
        let first_has_only_tool_results = result.first().map(|m| {
            m.content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        assert_ne!(
            first_has_only_tool_results,
            Some(true),
            "orphaned tool_result user message must have been stripped"
        );
    }

    /// Regression: when ALL messages in the window are tool round-trips (no plain
    /// user text), the old cascade-removal code would strip the orphaned tool_result
    /// AND the next assistant, making the following tool_result an orphan, and so on
    /// until the 2-message floor left a single orphaned tool_result at position 0.
    /// The fix inserts a placeholder user turn instead of cascading.
    #[test]
    fn test_sliding_window_all_tool_rounds_no_cascade_orphan() {
        use crate::claude::Message;

        // Build a conversation that is ENTIRELY tool round-trips:
        //   [0] user "query"               ← outside window (dropped by slice)
        //   [1] asst tool_use(A)           ← outside window
        //   [2] user tool_result(A)        ← window start → ORPHANED
        //   [3] asst tool_use(B)           ← valid pair start
        //   [4] user tool_result(B)        ← valid
        //   [5] asst tool_use(C)
        //   [6] user tool_result(C)
        //
        // window=5 keeps msgs[2..] = [orphan, asst(B), user(B), asst(C), user(C)]
        let make_tool_result_msg = |id: &str| Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "ok".to_string(),
                is_error: None,
            }],
        };
        let make_tool_use_msg = |id: &str| Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
            }],
        };

        let msgs: Vec<Message> = vec![
            user_msg("query"),         // [0] outside window
            make_tool_use_msg("A"),    // [1] outside window
            make_tool_result_msg("A"), // [2] orphaned boundary
            make_tool_use_msg("B"),    // [3] valid
            make_tool_result_msg("B"), // [4] valid
            make_tool_use_msg("C"),    // [5] valid
            make_tool_result_msg("C"), // [6] valid
        ];

        let result = apply_sliding_window(msgs, 5);

        // Must start with a user message.
        assert_eq!(
            result.first().unwrap().role,
            "user",
            "window must start with user"
        );
        // The first user message must NOT be a pure tool_result (no orphan).
        let first_is_tool_result_only = result.first().map(|m| {
            m.content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        assert_ne!(
            first_is_tool_result_only,
            Some(true),
            "orphaned tool_result must not be first: {:?}",
            result.first()
        );
        assert_eq!(
            result.first().unwrap().content[0].as_text(),
            Some("query"),
            "the dropped human request, not a synthetic placeholder, anchors retained tools"
        );
        // Valid tool rounds (B and C) must be preserved.
        assert!(
            result.len() >= 4,
            "valid tool round-trips B and C should be in window; got {} messages",
            result.len()
        );
    }

    /// Regression: orphaned tool_use (assistant sent tool_use but query was
    /// cancelled before tool_result was added). The final-pass validator in
    /// apply_sliding_window should strip the orphaned tool_use blocks, keeping
    /// any text content, so the conversation sent to the provider is clean.
    #[test]
    fn test_sliding_window_strips_orphaned_tool_use() {
        use crate::claude::Message;

        // Simulate a cancelled query: assistant wrote tool_uses but the
        // corresponding tool_result user message was never added.
        //   [0] user "query"
        //   [1] assistant: text + tool_use A  ← orphaned (no tool_result follows)
        //   [2] user "fix it"
        let msgs: Vec<Message> = vec![
            user_msg("query"),
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::Text {
                        text: "I'll analyze that.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_orphan".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/foo"}),
                    },
                ],
            },
            user_msg("fix it"),
        ];

        let result = apply_sliding_window(msgs, 20);

        // The orphaned tool_use block must be stripped; text content kept.
        for msg in &result {
            let has_orphaned_tool_use = msg.content.iter().any(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    id == "toolu_orphan"
                } else {
                    false
                }
            });
            assert!(
                !has_orphaned_tool_use,
                "orphaned tool_use must be stripped; got message: {:?}",
                msg
            );
        }

        // The text content ("I'll analyze that.") should be preserved.
        let has_text = result.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("I'll analyze")))
        });
        assert!(
            has_text,
            "text content of orphaned assistant message must be preserved"
        );

        // Window must start with a user message.
        assert_eq!(result.first().unwrap().role, "user");

        // "fix it" must still be present.
        let has_fix_it = result.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("fix it")))
        });
        assert!(has_fix_it, "user follow-up message must be preserved");
    }

    // ── tool_approval_summary ────────────────────────────────────────────────

    fn make_tool_use(name: &str, input: serde_json::Value) -> crate::tools::types::ToolUse {
        crate::tools::types::ToolUse {
            id: "test_id".to_string(),
            name: name.to_string(),
            input,
        }
    }

    #[test]
    fn test_tool_approval_summary_bash_with_command() {
        let tool = make_tool_use(
            "bash",
            serde_json::json!({"command": "git push origin main"}),
        );
        assert_eq!(
            tool_approval_summary(&tool),
            "Command: git push origin main"
        );
    }

    #[test]
    fn test_tool_approval_summary_bash_uppercase() {
        let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo test"}));
        assert_eq!(tool_approval_summary(&tool), "Command: cargo test");
    }

    #[test]
    fn test_tool_approval_summary_bash_long_command_truncated() {
        let long_cmd = "a".repeat(70);
        let tool = make_tool_use("bash", serde_json::json!({"command": long_cmd}));
        let result = tool_approval_summary(&tool);
        assert!(
            result.starts_with("Command: "),
            "should start with 'Command: ': {}",
            result
        );
        assert!(
            result.contains("..."),
            "long command should be truncated with '...': {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_bash_no_command() {
        let tool = make_tool_use("bash", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Execute shell command");
    }

    #[test]
    fn test_tool_approval_summary_read_with_path() {
        let tool = make_tool_use("read", serde_json::json!({"file_path": "src/main.rs"}));
        assert_eq!(tool_approval_summary(&tool), "File: src/main.rs");
    }

    #[test]
    fn test_tool_approval_summary_read_uppercase() {
        let tool = make_tool_use("Read", serde_json::json!({"file_path": "/a/b/c.rs"}));
        assert_eq!(tool_approval_summary(&tool), "File: /a/b/c.rs");
    }

    #[test]
    fn test_tool_approval_summary_read_no_path() {
        let tool = make_tool_use("read", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Read file");
    }

    #[test]
    fn test_tool_approval_summary_grep_with_pattern() {
        let tool = make_tool_use(
            "grep",
            serde_json::json!({"pattern": "fn main", "path": "src"}),
        );
        assert_eq!(tool_approval_summary(&tool), "Pattern: fn main");
    }

    #[test]
    fn test_tool_approval_summary_grep_long_pattern_truncated() {
        let long = "x".repeat(50);
        let tool = make_tool_use("grep", serde_json::json!({"pattern": long}));
        let result = tool_approval_summary(&tool);
        assert!(result.starts_with("Pattern: "), "got: {}", result);
        assert!(
            result.contains("..."),
            "long pattern should truncate: {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_grep_no_pattern() {
        let tool = make_tool_use("Grep", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Search files");
    }

    #[test]
    fn test_tool_approval_summary_glob_with_pattern() {
        let tool = make_tool_use("glob", serde_json::json!({"pattern": "**/*.rs"}));
        assert_eq!(tool_approval_summary(&tool), "Pattern: **/*.rs");
    }

    #[test]
    fn test_tool_approval_summary_glob_uppercase_no_pattern() {
        let tool = make_tool_use("Glob", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Find files");
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_with_reason() {
        let tool = make_tool_use(
            "EnterPlanMode",
            serde_json::json!({"reason": "Need to research the codebase"}),
        );
        assert_eq!(
            tool_approval_summary(&tool),
            "Reason: Need to research the codebase"
        );
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_long_reason_truncated() {
        let long_reason = "r".repeat(60);
        let tool = make_tool_use("EnterPlanMode", serde_json::json!({"reason": long_reason}));
        let result = tool_approval_summary(&tool);
        assert!(result.starts_with("Reason: "), "got: {}", result);
        assert!(
            result.contains("..."),
            "long reason should truncate: {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_no_reason() {
        let tool = make_tool_use("EnterPlanMode", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Enter planning mode");
    }

    #[test]
    fn test_tool_approval_summary_unknown_tool() {
        let tool = make_tool_use("WebFetch", serde_json::json!({"url": "https://docs.rs"}));
        assert_eq!(tool_approval_summary(&tool), "Execute WebFetch tool");
    }

    // ── dialog_result_to_confirmation (3-option Claude Code style) ───────────

    #[test]
    fn test_dialog_result_selected_0_approve_once() {
        // Option "1. Yes" → ApproveOnce
        let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(0), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
            ),
            "index 0 (Yes) should be ApproveOnce, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_selected_1_approve_pattern_session() {
        // Option "2. Yes, and don't ask again for: bash:*" → ApprovePatternSession
        let tool = make_tool_use("bash", serde_json::json!({"command": "git status"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(p.tool_name, "bash");
                assert_eq!(p.pattern, "*");
                assert!(
                    p.description.contains("session"),
                    "description: {}",
                    p.description
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_dialog_result_selected_2_deny() {
        // Option "3. No" → Deny
        let tool = make_tool_use("bash", serde_json::json!({"command": "rm -rf /"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "index 2 (No) should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_selected_high_index_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(99), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "out-of-range index should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_cancelled_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
        let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Cancelled, &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "Cancelled should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_custom_text_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
        let result = dialog_result_to_confirmation(
            crate::cli::tui::DialogResult::CustomText("please allow".to_string()),
            &tool,
        );
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "CustomText should be Deny (safety), got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_pattern_session_uses_tool_name() {
        // Verify the "don't ask again" pattern uses the actual tool name
        let tool = make_tool_use("grep", serde_json::json!({"pattern": "TODO"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(
                    p.tool_name, "grep",
                    "pattern tool_name should match tool: {}",
                    p.tool_name
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_pattern_session_tool_name_matches_tool_use() {
        // The pattern's tool_name must match the tool being approved —
        // otherwise the cache won't recognise future calls to the same tool.
        // Index 1 = "2. Yes, and don't ask again for: Bash:*"
        let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo fmt"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(
                    p.tool_name, "Bash",
                    "pattern tool_name should match ToolUse.name"
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_pattern_persistent_tool_name_matches_tool_use() {
        // Persistent approval is no longer in the 3-option dialog.
        // Index 2 → Deny; index 99 → Deny. Just verify nothing panics.
        let tool = make_tool_use("read", serde_json::json!({"file_path": "src/lib.rs"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "index 2 is No/Deny in 3-option dialog, got {:?}",
            result
        );
    }

}

/// Open `content` in `$VISUAL` or `$EDITOR` (falling back to `vi`), let the user
/// edit it, and return the saved result.  Suspends the terminal while the editor
/// runs and restores it afterwards.
fn open_in_editor(content: &str) -> anyhow::Result<String> {
    // Write proposed content to a temp file
    let tmp_path = std::env::temp_dir().join(format!("finch-edit-{}.txt", std::process::id()));
    std::fs::write(&tmp_path, content.as_bytes())?;

    // Prevent the asynchronous input/render tasks from writing while the
    // editor owns the terminal. The restorer is deliberately armed before
    // any terminal mutation so launch failures cannot strand Finch in the
    // editor's mode or alternate screen.
    crate::set_editor_active(true);
    struct TerminalRestorer;
    impl Drop for TerminalRestorer {
        fn drop(&mut self) {
            crate::tools::implementations::propose::resume_terminal_after_editor();
        }
    }
    let _restore = TerminalRestorer;
    crate::tools::implementations::propose::suspend_terminal_for_editor();

    let status = crate::tools::implementations::propose::run_editor(&tmp_path)?;

    if !status.success() {
        anyhow::bail!("Editor exited with status {}", status);
    }

    let edited = std::fs::read_to_string(&tmp_path)?;
    Ok(edited)
}
