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
use crate::cli::conversation::{ConversationHistory, ToolRoundProgress, ToolRoundToken};
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
use crate::review::store::DiffStore;
use crate::router::Router;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::ToolDefinition;

use super::events::{LlmRequest, ReplEvent, RunnerReconnectTarget};
use super::llm_loop::LlmLoop;
use super::model_selection::{activate_local_when_ready, LocalActivationOutcome, ModelSelection};
use super::query_processor::{refresh_context_strip, ActiveToolUsesMap};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_display::tool_result_to_display;
use super::tool_execution::ToolExecutionCoordinator;

// refresh_context_strip, dispatch_tool_uses, process_query_with_tools,
// ActiveToolUsesMap, and apply_sliding_window live in query_processor.rs.

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

async fn commit_tool_round_and_continue(
    conversation: &Arc<RwLock<ConversationHistory>>,
    query_id: Uuid,
    round_token: ToolRoundToken,
    llm_tx: &mpsc::UnboundedSender<LlmRequest>,
    checkpoint_path: Option<&std::path::Path>,
) -> std::result::Result<(), crate::cli::conversation::ToolRoundError> {
    let (admit_tx, admit_rx) = tokio::sync::oneshot::channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let (publication_tx, publication_rx) = tokio::sync::oneshot::channel();
    llm_tx
        .send(LlmRequest::Query {
            id: query_id,
            text: String::new(),
            no_tools: false,
            admission: Some(admit_rx),
            admission_ready: Some(ready_tx),
            spawned: Some(spawned_tx),
            publication: Some(publication_rx),
        })
        .map_err(|_| crate::cli::conversation::ToolRoundError::ContinuationUnavailable)?;
    tokio::time::timeout(std::time::Duration::from_secs(2), ready_rx)
        .await
        .map_err(|_| crate::cli::conversation::ToolRoundError::ContinuationUnavailable)?
        .map_err(|_| crate::cli::conversation::ToolRoundError::ContinuationUnavailable)?;
    let before_commit = {
        let mut history = conversation.write().await;
        let before_commit = history.clone();
        history.commit_tool_round(query_id, round_token)?;
        history.finalize_tool_round_commit();
        before_commit
    };
    let _ = admit_tx.send(());
    if !matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), spawned_rx).await,
        Ok(Ok(()))
    ) {
        *conversation.write().await = before_commit;
        return Err(crate::cli::conversation::ToolRoundError::ContinuationUnavailable);
    }
    if let Some(path) = checkpoint_path {
        let save_result = conversation.read().await.save(path);
        if let Err(error) = save_result {
            *conversation.write().await = before_commit;
            return Err(
                crate::cli::conversation::ToolRoundError::PersistenceUnavailable(error.to_string()),
            );
        }
    }
    if publication_tx.send(()).is_err() {
        return Err(crate::cli::conversation::ToolRoundError::ContinuationUnavailable);
    }
    Ok(())
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrainAttachmentRoute {
    LocalIpc {
        brain: String,
    },
    RemoteInvitation {
        target: crate::brain::remote::RemoteBrainTarget,
        invitation: String,
    },
}

fn brain_attachment_route(value: &str, invitation: Option<String>) -> Result<BrainAttachmentRoute> {
    if value.contains('@') {
        let invitation = invitation.context(
            "remote Brain attachments require `/brain join NAME@MACHINE[:PORT] INVITE`; use `/brain attach NAME` for a Brain on this daemon",
        )?;
        return Ok(BrainAttachmentRoute::RemoteInvitation {
            target: crate::brain::remote::RemoteBrainTarget::parse(value)?,
            invitation,
        });
    }
    anyhow::ensure!(
        invitation.is_none(),
        "Brain invitation targets must include NAME@MACHINE[:PORT]"
    );
    crate::brain::store::BrainStore::validate_name(value)?;
    Ok(BrainAttachmentRoute::LocalIpc {
        brain: value.to_string(),
    })
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

    /// Persona used for request-local provider system instructions.
    active_persona: Arc<RwLock<crate::config::Persona>>,

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
    agent_scheduler: Arc<crate::runtime::scheduler::AgentScheduler>,
    /// Revalidating provider resolver shared with child-agent model selection.
    provider_resolver: crate::runtime::scheduler::ProviderResolver,

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
        std::collections::HashMap<crate::brain::store::RunId, tokio_util::sync::CancellationToken>,

    /// Source/output already rendered while this frontend serviced its home
    /// Brain callback. Matching canonical events advance this marker without
    /// drawing a second copy in the runner console.
    local_brain_projections: std::collections::VecDeque<LocalBrainProjection>,

    /// Highest canonical revision already incorporated into the visible
    /// projection for each Brain. A watch snapshot and its buffered live tail
    /// can overlap; suppress that overlap here without changing the durable
    /// event log or hiding later lifecycle transitions.
    brain_projection_revisions: std::collections::HashMap<crate::brain::store::BrainId, u64>,

    /// Canonical tool calls replay into one grouped unit per Brain turn.
    remote_brain_tool_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
    /// Canonical run-correlated lifecycle rows. Program and result events for
    /// one RunId update this same selectable work unit instead of rendering as
    /// unrelated flat messages.
    remote_brain_run_units:
        std::collections::HashMap<crate::brain::store::RunId, RemoteBrainRunProjection>,
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
    daemon_ipc_error: Option<String>,

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

    /// Projection of the selected Brain task list shared with Todo tools.
    todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
    todo_journal_target: crate::tools::todo::TodoJournalTarget,

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
    home_runner_lease_id: Option<crate::brain::store::RunnerLeaseId>,
    /// Exact durable runner target, retained while its callback is offline.
    runner_reconnect_target: Option<RunnerReconnectTarget>,
    /// Exact Brain currently served by this frontend's ProgramRuntime. This
    /// starts as the home Brain but may change through an addressed handoff.
    runner_brain: Option<String>,
    /// Invalidates background renewal tasks when runner ownership moves.
    runner_renewal_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// Last runner-registration failure shown to the user. Lease renewal is
    /// periodic, so identical transport failures must not spam scrollback.
    last_home_runner_error: Option<String>,

    /// Generation of the authoritative home event watch.
    home_watch_epoch: u64,

    /// Last event-watch failure shown, tracked separately from runner health.
    last_home_watch_error: Option<String>,

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

    /// Local event channel for reviewed changesets.
    review_tx: tokio::sync::broadcast::Sender<crate::review::ReviewEvent>,

    /// In-memory store of pending diff proposals.
    diff_store: DiffStore,

    /// Receiver for local Diff/DiffEdit/DiffAccept/DiffReject events.
    review_rx: tokio::sync::broadcast::Receiver<crate::review::ReviewEvent>,

    // ── LLM worker loop channel ───────────────────────────────────────────
    /// Send LLM requests to the worker loop.
    llm_tx: mpsc::UnboundedSender<LlmRequest>,
    /// Receiver held until `run()` hands it off to `LlmLoop`.
    llm_rx: Option<mpsc::UnboundedReceiver<LlmRequest>>,
    #[cfg(test)]
    effect_audit_test_wrapper: Option<
        Arc<
            dyn Fn(
                    crate::server::RunnerEffectAuditControl,
                ) -> crate::server::RunnerEffectAuditControl
                + Send
                + Sync,
        >,
    >,
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
        if machine.is_empty() {
            "machine"
        } else {
            machine
        }
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

fn participant_display_name(subject: &str, local_machine: Option<&str>) -> String {
    let Some(machine) = local_machine else {
        return subject.to_string();
    };
    let Some((user, qualified)) = subject.split_once('@') else {
        return subject.to_string();
    };
    let Some(suffix) = qualified.strip_prefix(machine) else {
        return subject.to_string();
    };
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return subject.to_string();
    }
    format!("{user}{suffix}")
}

/// Strip the explicit model addressee used in collaborative Brain chatter.
/// Requiring whitespace (or end-of-input) avoids treating ordinary handles
/// such as `@finchbot` as model prompts.
fn finch_addressed_prompt(input: &str) -> Option<&str> {
    let input = input.trim();
    let suffix = input.strip_prefix("@finch")?;
    if !suffix.is_empty() && !suffix.starts_with(char::is_whitespace) {
        return None;
    }
    let prompt = suffix.trim();
    (!prompt.is_empty()).then_some(prompt)
}

fn approval_audience_summary(audience: &crate::brain::store::BrainApprovalAudience) -> String {
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
    audience: Option<&crate::brain::store::BrainApprovalAudience>,
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
    brain: String,
    run_id: crate::brain::store::RunId,
    response_tx: tokio::sync::oneshot::Sender<
        std::result::Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError>,
    >,
    /// Exact lifecycle order observed by this frontend while servicing the
    /// delegated turn. The daemon persists these as canonical Brain events.
    turn_events: Vec<crate::server::RunnerTurnEvent>,
    /// Execute-once VM effects are returned independently of the reducible
    /// checkpoint, including when the provider program ultimately fails.
    effect_journal: Vec<crate::server::RunnerEffectRecord>,
    /// Keep the correlation record until cancellation reaches a terminal VM
    /// boundary and all execute-once effects have been collected.
    cancellation_requested: bool,
    /// Provider tool calls that have been published for this turn but have
    /// not yet reached a result boundary. Cancellation retains the turn until
    /// this set is empty so late physical outcomes can be audited without
    /// publishing their ToolResult into conversation history.
    active_tool_ids: std::collections::HashSet<String>,
    approval_audience: crate::brain::store::BrainApprovalAudience,
    approval_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::server::RunnerApprovalRequest>>,
    /// Daemon-issued authority retained for the whole provider/tool loop.
    /// Query metadata carries a clone to each submitted ProgramRun.
    effect_audit: Option<crate::server::RunnerEffectAuditControl>,
    restart: Option<crate::tools::implementations::restart::DeferredFrontendRestart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedBrainToolResultDisposition {
    Publish,
    DiscardCancelled { quiesced: bool },
}

impl PendingNamedBrainTurn {
    fn observe_tool_calls(&mut self, tool_uses: Vec<crate::tools::types::ToolUse>) {
        for tool_use in tool_uses {
            self.active_tool_ids.insert(tool_use.id.clone());
            self.turn_events.push(crate::server::RunnerTurnEvent::Call {
                tool_id: tool_use.id,
                name: tool_use.name,
                input: tool_use.input,
            });
        }
    }

    fn observe_tool_result(
        &mut self,
        tool_id: &str,
        result: &anyhow::Result<String>,
    ) -> NamedBrainToolResultDisposition {
        if !self.cancellation_requested {
            return NamedBrainToolResultDisposition::Publish;
        }
        self.active_tool_ids.remove(tool_id);
        // Legacy effect receipts remain useful to the canonical turn commit,
        // even when cancellation forbids publishing the provider ToolResult.
        // The independent durable audit remains the execute-once authority.
        self.effect_journal
            .extend(runner_effect_records_from_tool_result(result));
        NamedBrainToolResultDisposition::DiscardCancelled {
            quiesced: self.active_tool_ids.is_empty(),
        }
    }
}

async fn resume_named_brain_program_boundaries(
    runtime: &crate::runtime::ProgramRuntime,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    control_tx: Option<mpsc::UnboundedSender<crate::server::RunnerProgramControlRequest>>,
    language: crate::brain::store::ProgramLanguage,
    interaction: crate::server::RunnerProgramInteraction,
    fixed_grant_ceiling: Option<crate::vm::EffectSet>,
    effects: std::sync::mpsc::Receiver<crate::runtime::VmEffectEnvelope>,
    mut outcome: crate::runtime::outcome::ExecutionOutcome,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    loop {
        outcome = match interaction {
            crate::server::RunnerProgramInteraction::Interactive => {
                super::query_processor::resume_interactive_boundaries(
                    runtime,
                    event_tx.clone(),
                    outcome,
                )
                .await?
            }
            crate::server::RunnerProgramInteraction::Noninteractive => {
                super::query_processor::resume_noninteractive_boundaries(runtime, outcome).await?
            }
        };
        let Some(crate::runtime::PendingTypedExecutionInfo {
            reason: crate::runtime::PendingTypedReason::AwaitingHostEffect { requirement },
            resume_effect_sequence: Some(sequence),
            ..
        }) = runtime.pending_typed_execution(outcome.execution_id)?
        else {
            return Ok(outcome);
        };
        if !matches!(
            requirement.capability,
            crate::vm::CapabilityKind::ScheduleCreate
                | crate::vm::CapabilityKind::ScheduleRead
                | crate::vm::CapabilityKind::ScheduleManage
        ) {
            return Ok(outcome);
        }
        let envelope = loop {
            let envelope = match effects.try_recv() {
                Ok(envelope) => envelope,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("named Brain schedule effect stream closed")
                }
            };
            if envelope.execution_id == outcome.execution_id && envelope.effect.sequence == sequence
            {
                break envelope;
            }
        };
        let response = match &control_tx {
            Some(control_tx) => match execute_named_brain_schedule_effect(
                runtime,
                control_tx,
                language,
                fixed_grant_ceiling.as_ref(),
                &envelope.effect,
            )
            .await
            {
                Ok(values) => crate::runtime::VmResumeResponse::Result { values },
                Err(error) => crate::runtime::VmResumeResponse::Denied {
                    reason: error.to_string(),
                },
            },
            None => crate::runtime::VmResumeResponse::Denied {
                reason: "named Brain schedule service is unavailable".into(),
            },
        };
        outcome = runtime
            .resume_vm_effect(crate::runtime::VmResume {
                execution_id: envelope.execution_id,
                sequence: envelope.effect.sequence,
                response,
            })
            .await?;
    }
}

async fn execute_named_brain_schedule_effect(
    runtime: &crate::runtime::ProgramRuntime,
    control_tx: &mpsc::UnboundedSender<crate::server::RunnerProgramControlRequest>,
    language: crate::brain::store::ProgramLanguage,
    fixed_grant_ceiling: Option<&crate::vm::EffectSet>,
    effect: &crate::vm::VmSideEffect,
) -> anyhow::Result<Vec<crate::vm::TypedValue>> {
    let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
        anyhow::bail!("schedule boundary did not carry a host request");
    };
    match effect.requirement.capability {
        crate::vm::CapabilityKind::ScheduleCreate => {
            let [crate::vm::TypedValue::String(source), crate::vm::TypedValue::Int(timestamp)] =
                arguments.as_slice()
            else {
                anyhow::bail!("schedule-create requires a callback and Unix timestamp");
            };
            let next_due_ms = u64::try_from(*timestamp)
                .ok()
                .and_then(|timestamp| timestamp.checked_mul(1_000))
                .ok_or_else(|| {
                    anyhow::anyhow!("schedule timestamp is outside the supported range")
                })?;
            let grant_ceiling = match fixed_grant_ceiling {
                Some(grant_ceiling) => grant_ceiling.clone(),
                None => runtime.effective_grants_for(None)?,
            };
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            control_tx
                .send(crate::server::RunnerProgramControlRequest::CreateSchedule {
                    language,
                    source: source.clone(),
                    grant_ceiling,
                    next_due_ms,
                    interval_ms: None,
                    delivery_policy: crate::brain::store::BrainScheduleDeliveryPolicy::Coalesce,
                    response_tx,
                })
                .map_err(|_| anyhow::anyhow!("named Brain schedule control disconnected"))?;
            let schedule = response_rx
                .await
                .map_err(|_| anyhow::anyhow!("named Brain schedule response was dropped"))?
                .map_err(anyhow::Error::msg)?;
            Ok(vec![crate::vm::TypedValue::Resource {
                kind: "schedule".into(),
                handle: schedule.schedule_id.0.to_string(),
                generation: 0,
            }])
        }
        crate::vm::CapabilityKind::ScheduleRead => {
            let schedule_id = schedule_id_argument(arguments)?;
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            control_tx
                .send(
                    crate::server::RunnerProgramControlRequest::InspectSchedule {
                        schedule_id,
                        response_tx,
                    },
                )
                .map_err(|_| anyhow::anyhow!("named Brain schedule control disconnected"))?;
            let schedule = response_rx
                .await
                .map_err(|_| anyhow::anyhow!("named Brain schedule response was dropped"))?
                .map_err(anyhow::Error::msg)?;
            let value = schedule.map(|schedule| {
                serde_json::json!({
                    "id": schedule.schedule_id.0,
                    "created_by": schedule.created_by,
                    "language": match schedule.language {
                        crate::brain::store::ProgramLanguage::Forth => "forth",
                        crate::brain::store::ProgramLanguage::Lisp => "lisp",
                    },
                    "next_due_ms": schedule.next_due_ms,
                    "interval_ms": schedule.interval_ms,
                    "active": schedule.active,
                })
            });
            Ok(vec![crate::vm::TypedValue::Option {
                inner_type: crate::vm::Type::Json,
                value: value.map(|value| Box::new(crate::vm::TypedValue::Json(value))),
            }])
        }
        crate::vm::CapabilityKind::ScheduleManage => {
            let schedule_id = schedule_id_argument(arguments)?;
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            control_tx
                .send(crate::server::RunnerProgramControlRequest::CancelSchedule {
                    schedule_id,
                    response_tx,
                })
                .map_err(|_| anyhow::anyhow!("named Brain schedule control disconnected"))?;
            Ok(vec![crate::vm::TypedValue::Bool(
                response_rx
                    .await
                    .map_err(|_| anyhow::anyhow!("named Brain schedule response was dropped"))?
                    .map_err(anyhow::Error::msg)?,
            )])
        }
        _ => anyhow::bail!("effect is not a named Brain schedule operation"),
    }
}

fn schedule_id_argument(
    arguments: &[crate::vm::TypedValue],
) -> anyhow::Result<crate::brain::store::ScheduleId> {
    let [crate::vm::TypedValue::Resource { kind, handle, .. }] = arguments else {
        anyhow::bail!("schedule operation requires one schedule resource");
    };
    anyhow::ensure!(kind == "schedule", "resource is not a schedule");
    Ok(crate::brain::store::ScheduleId(uuid::Uuid::parse_str(
        handle,
    )?))
}

#[derive(Clone)]
struct RemoteBrainApproval {
    client: crate::brain::remote::AttachedBrainClient,
    request_seq: u64,
    approval_id: String,
    audience: crate::brain::store::BrainApprovalAudience,
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

struct RemoteBrainRunProjection {
    unit: Arc<crate::cli::messages::WorkUnit>,
    status_row: usize,
    prompt_row: Option<usize>,
    program_row: Option<usize>,
    result_row: Option<usize>,
    tool_rows: std::collections::HashMap<String, usize>,
    approval_rows: std::collections::HashMap<String, usize>,
    locally_rendered_tool_ids: std::collections::HashSet<String>,
    locally_rendered_approval_ids: std::collections::HashSet<String>,
    locally_rendered_program: bool,
}

fn brain_run_group_label(
    run_id: crate::brain::store::RunId,
    kind: Option<crate::brain::store::BrainRunKind>,
) -> String {
    kind.map(|kind| format!("{kind:?} run {}", run_id.0))
        .unwrap_or_else(|| format!("Brain run {}", run_id.0))
}

fn ensure_remote_brain_run_projection<'a>(
    output_manager: &crate::cli::output_manager::OutputManager,
    projections: &'a mut std::collections::HashMap<
        crate::brain::store::RunId,
        RemoteBrainRunProjection,
    >,
    run_id: crate::brain::store::RunId,
    kind: Option<crate::brain::store::BrainRunKind>,
    status: crate::brain::store::BrainRunStatus,
) -> &'a mut RemoteBrainRunProjection {
    projections.entry(run_id).or_insert_with(|| {
        let label = brain_run_group_label(run_id, kind);
        let unit = output_manager.start_work_unit(&label);
        // WorkUnit's completed row presentation otherwise uses the generic
        // "Tools" title. Keep the canonical kind/id visible without
        // misclassifying lifecycle rows as model tool calls.
        unit.set_activity_presentation(&label);
        let status_row = unit.add_activity_row(format!("{label} · status"));
        unit.complete_row(status_row, format!("{status:?}").to_lowercase());
        RemoteBrainRunProjection {
            unit,
            status_row,
            prompt_row: None,
            program_row: None,
            result_row: None,
            tool_rows: std::collections::HashMap::new(),
            approval_rows: std::collections::HashMap::new(),
            locally_rendered_tool_ids: std::collections::HashSet::new(),
            locally_rendered_approval_ids: std::collections::HashSet::new(),
            locally_rendered_program: false,
        }
    })
}

/// Project a correlated event into its canonical RunId work unit. Snapshot
/// reattachment and live delivery share this path, so acknowledgement never
/// strips durable run contents from the shadow buffer.
fn project_remote_brain_run_event(
    output_manager: &crate::cli::output_manager::OutputManager,
    projections: &mut std::collections::HashMap<
        crate::brain::store::RunId,
        RemoteBrainRunProjection,
    >,
    event: &crate::brain::store::BrainEvent,
) -> bool {
    use crate::brain::store::{BrainEventKind, BrainRunKind, BrainRunStatus, ProgramLanguage};

    let Some(run_id) = event.run_id else {
        return false;
    };
    let (kind, status) = match &event.kind {
        BrainEventKind::RunStarted { run } => (Some(run.kind), run.status),
        BrainEventKind::SpeculativePrompt { .. } => (
            Some(BrainRunKind::Speculative),
            BrainRunStatus::QueuedForEnvironment,
        ),
        BrainEventKind::RunStatusChanged { status, .. } => (None, *status),
        BrainEventKind::ToolCall { .. }
        | BrainEventKind::ToolResult { .. }
        | BrainEventKind::ApprovalRequested { .. }
        | BrainEventKind::ApprovalDecided { .. }
        | BrainEventKind::Program { .. }
        | BrainEventKind::Result { .. } => (None, BrainRunStatus::Running),
        _ => return false,
    };
    let projection =
        ensure_remote_brain_run_projection(output_manager, projections, run_id, kind, status);

    match &event.kind {
        BrainEventKind::RunStarted { .. } => {}
        BrainEventKind::RunStatusChanged { status, detail, .. } => {
            let summary = detail
                .as_deref()
                .map(|detail| format!("{}: {detail}", format!("{status:?}").to_lowercase()))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());
            if *status == BrainRunStatus::Failed {
                projection.unit.fail_row(projection.status_row, summary);
            } else {
                projection.unit.complete_row(projection.status_row, summary);
            }
            if status.is_terminal() {
                projection.unit.set_complete();
            }
        }
        BrainEventKind::SpeculativePrompt { text } => {
            let row = *projection
                .prompt_row
                .get_or_insert_with(|| projection.unit.add_activity_row("prompt"));
            projection.unit.complete_row_with_body(
                row,
                "accepted",
                text.lines().map(str::to_owned).collect(),
            );
        }
        BrainEventKind::ToolCall {
            tool_id,
            name,
            input,
            ..
        } => {
            if projection.locally_rendered_tool_ids.contains(tool_id) {
                return true;
            }
            projection
                .tool_rows
                .entry(tool_id.clone())
                .or_insert_with(|| {
                    let input = input.to_string();
                    let input = if input.chars().count() > 80 {
                        format!("{}…", input.chars().take(79).collect::<String>())
                    } else {
                        input
                    };
                    projection.unit.add_row(format!("{name} {input}"))
                });
        }
        BrainEventKind::ToolResult {
            tool_id,
            output,
            is_error,
            ..
        } => {
            if projection.locally_rendered_tool_ids.contains(tool_id) {
                return true;
            }
            let row = *projection
                .tool_rows
                .entry(tool_id.clone())
                .or_insert_with(|| projection.unit.add_row(tool_id));
            if *is_error {
                projection.unit.fail_row(row, output);
            } else {
                let first = output.lines().next().unwrap_or_default();
                let summary = if first.chars().count() > 80 {
                    format!("{}…", first.chars().take(79).collect::<String>())
                } else {
                    first.to_string()
                };
                projection.unit.complete_row_with_body(
                    row,
                    summary,
                    output.lines().skip(1).map(str::to_owned).collect(),
                );
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
            if projection
                .locally_rendered_approval_ids
                .contains(approval_id)
            {
                return true;
            }
            if !projection.approval_rows.contains_key(approval_id) {
                let audience_summary = audience
                    .as_ref()
                    .map(|audience| {
                        format!(
                            "{} ({:?}, environment {})",
                            audience.subject, audience.role, audience.environment_generation
                        )
                    })
                    .unwrap_or_else(|| "legacy audience unspecified".to_string());
                let row = projection.unit.add_activity_row(format!(
                    "approval ({approval_kind}) for {audience_summary}: {subject}"
                ));
                for line in serde_json::to_string_pretty(detail)
                    .unwrap_or_else(|_| detail.to_string())
                    .lines()
                {
                    projection.unit.append_row_body_line(row, line.to_owned());
                }
                projection.approval_rows.insert(approval_id.clone(), row);
            }
        }
        BrainEventKind::ApprovalDecided {
            approval_id,
            decision,
            ..
        } => {
            if projection
                .locally_rendered_approval_ids
                .contains(approval_id)
            {
                return true;
            }
            let row = *projection
                .approval_rows
                .entry(approval_id.clone())
                .or_insert_with(|| {
                    projection
                        .unit
                        .add_activity_row(format!("approval {approval_id}"))
                });
            let choice = decision
                .get("choice")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("decided");
            let summary = format!("{choice} by {}", event.sender);
            if choice == "deny" {
                projection.unit.fail_row(row, summary);
            } else {
                projection.unit.complete_row(row, summary);
            }
        }
        BrainEventKind::Program { language, source } => {
            if projection.locally_rendered_program {
                return true;
            }
            let language = match language {
                ProgramLanguage::Forth => "Co-Forth",
                ProgramLanguage::Lisp => "Lisp",
            };
            let row = *projection.program_row.get_or_insert_with(|| {
                projection
                    .unit
                    .add_activity_row(format!("{language} program"))
            });
            projection.unit.complete_row_with_body(
                row,
                format!("event #{}", event.seq),
                source.lines().map(str::to_owned).collect(),
            );
        }
        BrainEventKind::Result { output, error, .. } => {
            let row = *projection
                .result_row
                .get_or_insert_with(|| projection.unit.add_activity_row("result"));
            if let Some(error) = error {
                projection.unit.fail_row(row, error);
            } else {
                projection.unit.complete_row_with_body(
                    row,
                    "completed",
                    output.lines().map(str::to_owned).collect(),
                );
            }
        }
        _ => unreachable!("correlated run event was filtered above"),
    }
    true
}

struct LocalBrainProjection {
    run_id: crate::brain::store::RunId,
    source: String,
    output: String,
    tool_ids: std::collections::HashSet<String>,
    approval_ids: std::collections::HashSet<String>,
    program_seq: Option<u64>,
    transient_output_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
    failed: bool,
}

fn failed_local_brain_projection(
    run_id: crate::brain::store::RunId,
    turn_events: &[crate::server::RunnerTurnEvent],
    transient_output_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
) -> LocalBrainProjection {
    let tool_ids = turn_events
        .iter()
        .filter_map(|event| match event {
            crate::server::RunnerTurnEvent::Call { tool_id, .. }
            | crate::server::RunnerTurnEvent::Result { tool_id, .. } => Some(tool_id.clone()),
            _ => None,
        })
        .collect();
    let approval_ids = turn_events
        .iter()
        .filter_map(|event| match event {
            crate::server::RunnerTurnEvent::ApprovalRequested { approval_id, .. }
            | crate::server::RunnerTurnEvent::ApprovalDecided { approval_id, .. } => {
                Some(approval_id.clone())
            }
            _ => None,
        })
        .collect();
    LocalBrainProjection {
        run_id,
        source: String::new(),
        output: String::new(),
        tool_ids,
        approval_ids,
        program_seq: None,
        transient_output_unit,
        failed: true,
    }
}

fn register_named_brain_turn_projection(
    projections: &mut std::collections::VecDeque<LocalBrainProjection>,
    run_id: crate::brain::store::RunId,
    result: &std::result::Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError>,
    transient_output_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
) {
    match result {
        Ok(result) => {
            let tool_ids = result
                .turn_events
                .iter()
                .filter_map(|event| match event {
                    crate::server::RunnerTurnEvent::Call { tool_id, .. }
                    | crate::server::RunnerTurnEvent::Result { tool_id, .. } => {
                        Some(tool_id.clone())
                    }
                    _ => None,
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
                    _ => None,
                })
                .collect();
            projections.push_back(LocalBrainProjection {
                run_id,
                source: result.source.clone(),
                output: result.output.clone(),
                tool_ids,
                approval_ids,
                program_seq: None,
                transient_output_unit,
                failed: false,
            });
        }
        Err(error) => projections.push_back(failed_local_brain_projection(
            run_id,
            &error.turn_events,
            transient_output_unit,
        )),
    }
}

fn named_brain_wire_source(
    messages: Vec<crate::claude::Message>,
    initial_message_count: usize,
) -> anyhow::Result<(
    String,
    crate::brain::store::ProgramLanguage,
    Vec<crate::claude::Message>,
)> {
    anyhow::ensure!(
        initial_message_count <= messages.len(),
        "named Brain conversation changed before durable continuation capture"
    );
    let continuation_messages = messages[initial_message_count..].to_vec();
    anyhow::ensure!(
        continuation_messages
            .iter()
            .all(|message| matches!(message.role.as_str(), "assistant" | "user")),
        "named Brain continuation messages were invalid"
    );
    let assistant = continuation_messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .ok_or_else(|| anyhow::anyhow!("named Brain turn produced no wire source"))?;
    let source = assistant
        .content
        .iter()
        .filter_map(crate::claude::ContentBlock::as_text)
        .collect::<String>();
    anyhow::ensure!(
        !source.trim().is_empty(),
        "named Brain turn produced no wire source"
    );
    let language = match crate::programs::ProgramLanguage::infer_wire_source(&source)? {
        crate::programs::ProgramLanguage::Forth => crate::brain::store::ProgramLanguage::Forth,
        crate::programs::ProgramLanguage::Lisp => crate::brain::store::ProgramLanguage::Lisp,
    };
    Ok((source, language, continuation_messages))
}

#[allow(clippy::too_many_arguments)]
fn assemble_named_brain_turn(
    projections: &mut std::collections::VecDeque<LocalBrainProjection>,
    run_id: crate::brain::store::RunId,
    messages: anyhow::Result<Vec<crate::claude::Message>>,
    program_runtime: &crate::runtime::ProgramRuntime,
    output: String,
    turn_events: Vec<crate::server::RunnerTurnEvent>,
    effect_journal: Vec<crate::server::RunnerEffectRecord>,
    commit_ack: Option<crate::server::RunnerTurnCommitAck>,
    transient_output_unit: Option<Arc<crate::cli::messages::WorkUnit>>,
    invocation_metadata: Option<crate::providers::types::InvocationMetadata>,
    initial_message_count: usize,
) -> std::result::Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError> {
    let result = (|| -> anyhow::Result<crate::server::RunnerTurnResult> {
        let (source, language, continuation_messages) =
            named_brain_wire_source(messages?, initial_message_count)?;
        let runtime_revision = program_runtime.revision();
        let checkpoint = program_runtime
            .revision_history()?
            .into_iter()
            .find(|snapshot| snapshot.revision == runtime_revision)
            .and_then(|snapshot| snapshot.checkpoint)
            .ok_or_else(|| {
                anyhow::anyhow!("named Brain revision {runtime_revision} is not checkpointable")
            })?;
        Ok(crate::server::RunnerTurnResult {
            source,
            language,
            output,
            continuation_messages,
            invocation_metadata,
            turn_events: turn_events.clone(),
            runtime_revision,
            checkpoint,
            effect_journal: effect_journal.clone(),
            commit_ack,
        })
    })()
    .map_err(|error| crate::server::RunnerTurnError {
        message: error.to_string(),
        turn_events,
        effect_journal,
    });
    register_named_brain_turn_projection(projections, run_id, &result, transient_output_unit);
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalProjectionMatch {
    None,
    Suppress,
    SuppressAndComplete,
}

impl LocalBrainProjection {
    fn observe(&mut self, event: &crate::brain::store::BrainEvent) -> LocalProjectionMatch {
        if event.run_id != Some(self.run_id) {
            return LocalProjectionMatch::None;
        }
        match &event.kind {
            crate::brain::store::BrainEventKind::ToolCall { tool_id, .. }
            | crate::brain::store::BrainEventKind::ToolResult { tool_id, .. }
                if self.tool_ids.contains(tool_id) =>
            {
                LocalProjectionMatch::Suppress
            }
            crate::brain::store::BrainEventKind::ApprovalRequested { approval_id, .. }
            | crate::brain::store::BrainEventKind::ApprovalDecided { approval_id, .. }
                if self.approval_ids.contains(approval_id) =>
            {
                LocalProjectionMatch::Suppress
            }
            crate::brain::store::BrainEventKind::Program { source, .. }
                if event.sender == "provider"
                    && self.program_seq.is_none()
                    && self.source == *source =>
            {
                self.program_seq = Some(event.seq);
                LocalProjectionMatch::Suppress
            }
            crate::brain::store::BrainEventKind::Result {
                request_seq,
                output,
                error,
                ..
            } if self.program_seq == Some(*request_seq)
                && error.is_none()
                && self.output == *output =>
            {
                LocalProjectionMatch::SuppressAndComplete
            }
            crate::brain::store::BrainEventKind::Result { error: Some(_), .. } if self.failed => {
                LocalProjectionMatch::SuppressAndComplete
            }
            _ => LocalProjectionMatch::None,
        }
    }
}

fn project_remote_brain_live_run_event(
    output_manager: &crate::cli::output_manager::OutputManager,
    projections: &mut std::collections::HashMap<
        crate::brain::store::RunId,
        RemoteBrainRunProjection,
    >,
    local_projections: &mut std::collections::VecDeque<LocalBrainProjection>,
    selected_brain_is_home: bool,
    event: &crate::brain::store::BrainEvent,
) -> bool {
    if event.run_id.is_none() {
        return false;
    }
    let projection_match = selected_brain_is_home
        .then(|| local_projections.front_mut())
        .flatten()
        .map(|projection| projection.observe(event))
        .unwrap_or(LocalProjectionMatch::None);
    if projection_match != LocalProjectionMatch::None {
        if let Some(projection) = projections.get_mut(&event.run_id.expect("checked above")) {
            match &event.kind {
                crate::brain::store::BrainEventKind::ToolCall { tool_id, .. }
                | crate::brain::store::BrainEventKind::ToolResult { tool_id, .. } => {
                    projection.locally_rendered_tool_ids.insert(tool_id.clone());
                }
                crate::brain::store::BrainEventKind::ApprovalRequested { approval_id, .. }
                | crate::brain::store::BrainEventKind::ApprovalDecided { approval_id, .. } => {
                    projection
                        .locally_rendered_approval_ids
                        .insert(approval_id.clone());
                }
                crate::brain::store::BrainEventKind::Program { .. } => {
                    projection.locally_rendered_program = true;
                }
                _ => {}
            }
        }
    }
    let projected = project_remote_brain_run_event(output_manager, projections, event);
    if projected && projection_match == LocalProjectionMatch::SuppressAndComplete {
        if let Some(local) = local_projections.pop_front() {
            if let Some(output_unit) = local.transient_output_unit {
                output_manager
                    .remove_message(crate::cli::messages::Message::id(output_unit.as_ref()));
            }
        }
    }
    projected
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

fn runner_effect_records_from_tool_result(
    result: &anyhow::Result<String>,
) -> Vec<crate::server::RunnerEffectRecord> {
    result
        .as_ref()
        .ok()
        .and_then(|output| {
            serde_json::from_str::<crate::runtime::outcome::ExecutionOutcome>(output).ok()
        })
        .map(|outcome| super::query_processor::runner_effect_records(&outcome))
        .unwrap_or_default()
}

fn deferred_proposal_from_tool_result(result: &anyhow::Result<String>) -> Option<DeferredProposal> {
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
    let [crate::vm::TypedValue::String(language), crate::vm::TypedValue::String(intent), crate::vm::TypedValue::String(source)] =
        arguments.as_slice()
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
mod deferred_proposal_tests;

mod brain;

mod tools;

mod plan;

mod commands;

mod dispatch;

mod input;

impl EventLoop {
    fn start_llm_worker(&mut self) {
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
            Arc::clone(&self.active_persona),
            self.session_label.clone(),
            self.cwd.clone(),
            self.context_lines,
            self.max_verbatim_messages,
            self.context_recall_k,
            self.streaming_enabled,
            self.enable_summarization,
            self.auto_compact_enabled,
            self.metrics_logger.clone(),
        );
        tokio::spawn(llm_loop.run());
    }

    /// Create a new event loop with unified generators
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Arc<RwLock<ConversationHistory>>,
        active_persona: Arc<RwLock<crate::config::Persona>>,
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
        daemon_ipc_error: Option<String>,
        mode: Arc<RwLock<ReplMode>>,
        memory_system: Option<Arc<crate::memory::MemorySystem>>,
        session_label: String,
        session_uuid: Uuid,
        available_providers: Vec<crate::config::ProviderEntry>,
        active_provider_index: usize,
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
        context_lines: usize,
        max_verbatim_messages: usize,
        context_recall_k: usize,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
        todo_journal_target: crate::tools::todo::TodoJournalTarget,
        todo_journal_receiver: crate::tools::todo::TodoJournalReceiver,
        enable_summarization: bool,
        auto_compact_enabled: bool,
        daemon_base_url: Option<String>,
        provider_resolver: crate::runtime::scheduler::ProviderResolver,
        agent_scheduler: Arc<crate::runtime::scheduler::AgentScheduler>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        todo_journal_receiver.spawn();
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
        tui_renderer.set_task_rows(crate::cli::repl_event::activity_view::TodoRows::new(
            Arc::clone(&todo_list),
        ));
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

        // Unit-level production-boundary fixtures drive the event channel
        // directly and must not install a competing terminal reader.
        #[cfg(not(test))]
        let input_rx = {
            let rx = spawn_input_task(Arc::clone(&tui_renderer), quit_tx);
            // Keys are buffered from here, but nothing acts on them until the
            // select loop below. The two instants are recorded separately
            // because they are routinely confused (#364).
            crate::startup::mark(crate::startup::MARK_INPUT_CAPTURED);
            rx
        };
        #[cfg(test)]
        let input_rx = {
            drop(quit_tx);
            let (_input_tx, input_rx) = mpsc::unbounded_channel();
            input_rx
        };

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

        let (review_tx, review_rx) =
            tokio::sync::broadcast::channel::<crate::review::ReviewEvent>(128);

        let participant_subject = local_participant_subject();
        let runner_subject = runner_subject_from(&participant_subject, Uuid::new_v4());

        Self {
            event_rx,
            event_tx,
            input_rx,
            conversation,
            active_persona,
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
            agent_scheduler,
            provider_resolver,
            active_query_id: Arc::new(RwLock::new(None)),
            pending_queries: std::collections::VecDeque::new(),
            pending_named_brain_turns: std::collections::HashMap::new(),
            pending_named_brain_programs: std::collections::HashMap::new(),
            local_brain_projections: std::collections::VecDeque::new(),
            brain_projection_revisions: std::collections::HashMap::new(),
            remote_brain_tool_unit: None,
            remote_brain_run_units: std::collections::HashMap::new(),
            remote_brain_tool_rows: std::collections::HashMap::new(),
            remote_brain_approval_rows: std::collections::HashMap::new(),
            queued_remote_brain_approvals: std::collections::VecDeque::new(),
            active_remote_brain_approval: None,
            pending_approvals: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_vm_approval: None,
            ipc_client,
            daemon_ipc_error,
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
            session_uuid,
            cwd: String::new(), // populated at the start of run()
            context_lines,
            max_verbatim_messages,
            context_recall_k,
            todo_list,
            todo_journal_target,
            enable_summarization,
            auto_compact_enabled,
            pending_dialog_tx: None,
            pending_poset_run: None,
            tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
            current_graph: Arc::new(tokio::sync::Mutex::new(crate::graph::ExecutionGraph::new())),
            stack,
            poset,
            plan_word: None,
            review_tx,
            diff_store: DiffStore::new(),
            review_rx,
            active_remote_brain: None,
            home_brain: None,
            home_runner_lease_active: false,
            home_runner_lease_id: None,
            runner_reconnect_target: None,
            runner_brain: None,
            runner_renewal_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_home_runner_error: None,
            home_watch_epoch: 0,
            last_home_watch_error: None,
            daemon_base_url,
            llm_tx,
            llm_rx: Some(llm_rx),
            #[cfg(test)]
            effect_audit_test_wrapper: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn start_named_brain_test_runner(
        mut self,
        brain: String,
    ) -> (
        mpsc::UnboundedSender<ReplEvent>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        self.runner_brain = Some(brain);
        self.home_runner_lease_active = true;
        self.start_llm_worker();
        let event_tx = self.event_tx.clone();
        let driver = tokio::task::spawn_local(async move {
            while let Some(event) = self.event_rx.recv().await {
                if matches!(&event, ReplEvent::Shutdown) {
                    break;
                }
                self.handle_event(event).await?;
            }
            Ok(())
        });
        (event_tx, driver)
    }

    #[cfg(test)]
    pub(crate) fn set_effect_audit_test_wrapper(
        &mut self,
        wrapper: Arc<
            dyn Fn(
                    crate::server::RunnerEffectAuditControl,
                ) -> crate::server::RunnerEffectAuditControl
                + Send
                + Sync,
        >,
    ) {
        self.effect_audit_test_wrapper = Some(wrapper);
    }

    #[cfg(test)]
    pub(crate) fn conversation_for_test(&self) -> Arc<RwLock<ConversationHistory>> {
        Arc::clone(&self.conversation)
    }

    /// Run the event loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::debug!("Event loop starting");
        // Signal that the TUI owns the terminal so proposal editors perform a
        // complete terminal-protocol handoff before launching $VISUAL/$EDITOR.
        //
        // This and the generator resolve below were the rest of the 6.4 ms the
        // first report could not attribute (#364).
        {
            let _phase = crate::startup::phase(crate::startup::PHASE_TUI_HANDOFF);
            crate::set_tui_active(true);

            // ── Startup header (Claude Code style) ───────────────────────────
            // Clear accumulated startup noise from the output manager, then
            // print a clean header: finch version · primary model · cwd.
            self.output_manager.clear();
        }

        let model_name = {
            let _phase = crate::startup::phase(crate::startup::PHASE_GENERATOR_RESOLVE);
            self.model_selection.generator().await.name().to_string()
        };
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
        let home_runner_state = {
            // Four serial IPC round-trips, before the first frame (#364).
            let mut phase = crate::startup::phase(crate::startup::PHASE_BRAIN_REGISTER);
            match self.register_home_brain().await {
                Ok(state) => {
                    // Brains actually registered, so the offline path reports
                    // zero rather than claiming one (#364).
                    phase.detail(if state.is_some() {
                        crate::startup::PhaseDetail::count(1).with_category("registered")
                    } else {
                        crate::startup::PhaseDetail::count(0).with_category("offline")
                    });
                    state
                }
                Err(error) => {
                    phase.detail(crate::startup::PhaseDetail::count(0).with_category("failed"));
                    tracing::warn!("could not register home Brain: {error}");
                    None
                }
            }
        };
        // Runner-state projection and the header itself. Small individually
        // and 1.1 ms together on the reference machine -- which is what the
        // report's `unaccounted_ms` was for, and why this now has a name
        // (#364).
        let header_phase = crate::startup::phase(crate::startup::PHASE_STARTUP_HEADER);
        self.home_runner_lease_id = home_runner_state
            .as_ref()
            .and_then(|state| state.target.lease_id);
        self.home_runner_lease_active = home_runner_state
            .as_ref()
            .is_some_and(|state| state.registration.is_ok());
        self.runner_reconnect_target = home_runner_state.as_ref().map(|state| state.target.clone());
        self.runner_brain = home_runner_state.as_ref().and_then(|state| {
            state
                .registration
                .is_ok()
                .then(|| state.target.brain.clone())
        });
        let home_runner_error = home_runner_state
            .as_ref()
            .and_then(|state| state.registration.as_ref().err())
            .cloned();
        self.last_home_runner_error = home_runner_error.clone();
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            match &home_runner_state {
                Some(state) if state.registration.is_ok() => {
                    format!("◆ brain: {} · runner", state.target.brain)
                }
                Some(_) => {
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
        drop(header_phase);
        // Queued, not painted: in TUI mode stdout is disabled and `write_info`
        // appends to an in-memory buffer, so the first real paint happens on a
        // render tick after the loop below starts (#364).
        crate::startup::mark(crate::startup::MARK_HEADER_QUEUED);
        if let Some(error) = self.daemon_ipc_error.take() {
            self.output_manager
                .write_info(format!("Brain daemon unavailable: {error}"));
        }
        if let Some(error) = home_runner_error {
            self.output_manager.write_info(format!(
                "{}: runner unavailable: {}",
                self.session_label, error
            ));
            let epoch = self
                .runner_renewal_epoch
                .load(std::sync::atomic::Ordering::SeqCst);
            if let Some(target) = self.runner_reconnect_target.clone() {
                self.schedule_home_runner_reconnect(epoch, 0, target);
            }
        }
        if self.daemon_base_url.is_some() {
            // One Brain, whatever the size of the on-disk inventory: the
            // frontend attaches its own home Brain and never enumerates the
            // Brain root (#364).
            let mut phase = crate::startup::phase(crate::startup::PHASE_BRAIN_ATTACH);
            phase.detail(crate::startup::PhaseDetail::count(1).with_category("attached"));
            if let Err(error) = self.attach_home_brain().await {
                phase.detail(crate::startup::PhaseDetail::count(0).with_category("failed"));
                let detail = error.to_string();
                self.last_home_watch_error = Some(detail.clone());
                self.output_manager.write_info(format!(
                    "{}: home event watch unavailable: {}; reconnecting independently of the runner callback",
                    self.session_label, detail
                ));
                self.schedule_home_brain_reconnect(self.home_watch_epoch, 0);
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        // Show weekly license notice for non-commercial users (honor system)
        {
            let _phase = crate::startup::phase(crate::startup::PHASE_LICENSE_NOTICE);
            use crate::config::{load_config, LicenseType};
            // The third of four full reads and TOML parses of `config.toml` on
            // an interactive start. It was inside the unattributed gap, so the
            // report showed a reader half the config cost (#364).
            let loaded = {
                let mut phase = crate::startup::phase(crate::startup::PHASE_CONFIG);
                phase.detail(crate::startup::PhaseDetail::category("license_notice"));
                load_config()
            };
            if let Ok(cfg) = loaded {
                if cfg.license.license_type == LicenseType::Noncommercial {
                    let today = chrono::Local::now().date_naive();
                    // Recorded in a runtime-state file, not in `config.toml`.
                    // This runs on every start, and writing the user's config
                    // to remember that a notice was shown rewrote a file they
                    // never asked to change -- reformatting it, dropping
                    // comments, moving its mtime (#76, "Keep ordinary Finch
                    // startup byte-for-byte read-only on user
                    // configuration").
                    let should_show = crate::config::claim_notice_showing_now(
                        cfg.license.notice_suppress_until.as_deref(),
                        today,
                    );
                    // What is and is not covered, stated exactly, because an
                    // earlier version of this note claimed more and named a
                    // function that no longer exists.
                    //
                    // `tests/startup_is_readonly_on_config.rs` proves
                    // `claim_notice_showing_now` writes no config. It does NOT
                    // cover this block: a `cfg.save()` added anywhere in here
                    // reships #76 -- the read-only-startup guarantee --
                    // with a green suite, because nothing calls
                    // `EventLoop::run` outside production. So do not add one.
                    // If this block ever needs to persist something, put it
                    // behind a function the integration test can call.
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
                    }
                }
            }
        }

        // Priming the status bar: compaction state, plan mode, the memory
        // engine badge and the context strip. All before the first frame, and
        // all previously unattributed (#364).
        let status_prime_phase = crate::startup::phase(crate::startup::PHASE_STATUS_PRIME);

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
        drop(status_prime_phase);

        // ── Spawn LLM worker loop ─────────────────────────────────────────────
        // Hand the receiver half of the channel to LlmLoop so it runs as its own
        // Tokio task, decoupled from TUI select! timing.
        let llm_worker_phase = crate::startup::phase(crate::startup::PHASE_LLM_WORKER);
        self.start_llm_worker();

        // Render interval (33ms ≈ 30fps) — smooth streaming without terminal flicker.
        // 60fps (16ms) caused visual artifacts on most terminal emulators; 30fps is the
        // sweet spot: fast enough to feel live, slow enough to not tear.
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));

        // Cleanup interval (30 seconds)
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        // Flag to control the loop
        let mut should_exit = false;
        drop(llm_worker_phase);

        // Time-to-ready ends here: the first instant at which a typed key is
        // acted upon rather than merely buffered. Not the first painted frame
        // -- that happens on a render tick after this loop starts, which is
        // why the mark above it is called `header_queued` (#364).
        crate::startup::ready();

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
                        ReplEvent::VmEffectJournalComplete { .. } => {
                            "VmEffectJournalComplete"
                        }
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
                        ReplEvent::HomeBrainMessage { .. } => "HomeBrainMessage",
                        ReplEvent::HomeBrainWatchFailed { .. } => "HomeBrainWatchFailed",
                        ReplEvent::ReconnectHomeBrain { .. } => "ReconnectHomeBrain",
                        ReplEvent::ReconnectHomeRunner { .. } => "ReconnectHomeRunner",
                        ReplEvent::RunnerLeaseStatus { .. } => "RunnerLeaseStatus",
                        ReplEvent::NamedBrainProgramRequested(_) => "NamedBrainProgramRequested",
                        ReplEvent::NamedBrainTurnRequested(_) => "NamedBrainTurnRequested",
                        ReplEvent::NamedBrainMemoryProjectionRequested(_) => {
                            "NamedBrainMemoryProjectionRequested"
                        }
                        ReplEvent::NamedBrainRunCancelRequested(_) => {
                            "NamedBrainRunCancelRequested"
                        }
                        ReplEvent::NamedBrainProgramFinished(_) => "NamedBrainProgramFinished",
                        ReplEvent::FrontendRestartReady { .. } => "FrontendRestartReady",
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
                                        .push(crate::brain::store::BrainEventKind::ApprovalDecided {
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

                // Structured diff-review events (proposals, edits, accepts, rejects)
                ev = self.review_rx.recv() => {
                    match ev {
                        Ok(crate::review::ReviewEvent::Diff { id, label, patch, description }) => {
                            let proposed_by = "model".to_string();
                            self.diff_store.propose(id, label.clone(), patch.clone(), description.clone(), proposed_by.clone());
                            self.render_diff_proposal(id, &label, &patch, description.as_deref(), &proposed_by);
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after Diff proposal failed: {e}");
                            }
                        }
                        Ok(crate::review::ReviewEvent::DiffEdit { diff_id, patch, description }) => {
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
                        Ok(crate::review::ReviewEvent::DiffAccept { diff_id }) => {
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
                        Ok(crate::review::ReviewEvent::DiffReject { diff_id, reason }) => {
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
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // A newer review update will trigger another redraw.
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
                .push_remote_brain(crate::brain::store::BrainEventKind::Prompt { text: input })
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
        let pending_image_entries: Vec<(usize, String, String)> = {
            let mut tui = self.tui_renderer.lock().await;
            tui.pending_images.drain(..).collect()
        };
        let pending_images: Vec<(String, String)> = pending_image_entries
            .iter()
            .map(|(_, b64, media_type)| (media_type.clone(), b64.clone()))
            .collect();

        // Echo query to output buffer (skip when caller already echoed)
        if echo {
            self.output_manager.write_user(input.clone());
        }

        // Create a new query
        let conversation_snapshot = self.conversation.read().await.snapshot();
        let query_id = self.query_states.create_query(conversation_snapshot).await;

        // Build and durably checkpoint the next provider-visible history
        // before publishing it in memory or dispatching provider work.
        let mut proposed_history = self.conversation.read().await.clone();
        if pending_images.is_empty() {
            proposed_history.add_user_message(input.clone());
        } else {
            proposed_history.add_user_message_with_images(input.clone(), &pending_images);
        }
        if let Err(error) = self.checkpoint_history(&proposed_history) {
            self.query_states.remove_query(query_id).await;
            self.tui_renderer
                .lock()
                .await
                .pending_images
                .extend(pending_image_entries);
            self.report_checkpoint_error(
                "Query was not sent; its conversation checkpoint could not be written",
                &error,
            );
            return Ok(());
        }
        *self.conversation.write().await = proposed_history;

        // Update compaction percentage in status bar
        self.update_compaction_status().await;

        // Set as active query (for cancellation)
        *self.active_query_id.write().await = Some(query_id);

        // Send query to the LLM worker loop (no tools for chat_only word-push responses)
        let _ = self.llm_tx.send(LlmRequest::Query {
            id: query_id,
            text: input,
            no_tools: chat_only,
            admission: None,
            admission_ready: None,
            spawned: None,
            publication: None,
        });

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
        let query_id = self
            .pending_named_brain_turns
            .iter()
            .find_map(|(query_id, turn)| (turn.run_id == request.run_id).then_some(*query_id));
        let Some(query_id) = query_id else {
            let _ = request.response_tx.send(Ok(false));
            return;
        };
        if !self.query_states.cancel_query(query_id).await {
            let _ = request.response_tx.send(Ok(false));
            return;
        }
        self.conversation.write().await.abort_staged(query_id);
        self.close_active_tool_rows(query_id, "cancelled remotely")
            .await;
        if let Some(pending) = self.pending_named_brain_turns.get_mut(&query_id) {
            pending.cancellation_requested = true;
        }
        let _ = request.response_tx.send(Ok(true));
    }

    /// Whether this runner can project ANY memory right now.
    ///
    /// Both conditions are systemic, not about one turn: a replay pass walks
    /// every completed run in the Brain and each of them fails identically, so
    /// each error carries `RUNNER_UNAVAILABLE_PREFIX` and the pass aborts at the
    /// first rather than paying an IPC round trip and a log line per run under
    /// the Brain's execution lock.
    ///
    /// Split out from `project_named_brain_memory` so the prefix is testable:
    /// constructing a whole `EventLoop` is impractical, and a test that supplies
    /// the prefix as its own literal pins the consumer while leaving the
    /// producer free to stop emitting it.
    fn runner_can_project_memory(
        runner_brain: Option<&str>,
        lease_active: bool,
        memory_enabled: bool,
        brain: &str,
    ) -> anyhow::Result<()> {
        if runner_brain != Some(brain) || !lease_active {
            anyhow::bail!(
                "{}frontend does not hold the runner lease for named Brain '{brain}'",
                crate::server::RUNNER_UNAVAILABLE_PREFIX
            );
        }
        anyhow::ensure!(
            memory_enabled,
            "{}memory is disabled on the environment runner",
            crate::server::RUNNER_UNAVAILABLE_PREFIX
        );
        Ok(())
    }

    async fn project_named_brain_memory(
        &self,
        request: crate::server::RunnerMemoryProjectionRequest,
    ) {
        let result = async {
            Self::runner_can_project_memory(
                self.runner_brain.as_deref(),
                self.home_runner_lease_active,
                self.memory_system.is_some(),
                &request.brain,
            )?;
            let memory = self
                .memory_system
                .as_ref()
                .expect("checked by runner_can_project_memory");
            let provenance = crate::memory::BrainConversationProvenance {
                brain_id: request.brain_id.0.to_string(),
                run_id: request.run_id.0.to_string(),
                request_seq: request.request_seq,
            };
            let mut inserted = 0;
            for (role, content) in [
                ("user", request.prompt.as_str()),
                ("assistant", request.rendered.as_str()),
            ] {
                if !content.trim().is_empty()
                    && memory
                        .insert_brain_conversation(
                            role,
                            content,
                            None,
                            Some(&request.brain),
                            &provenance,
                        )
                        .await?
                {
                    inserted += 1;
                }
            }
            Ok::<usize, anyhow::Error>(inserted)
        }
        .await
        .map_err(|error| error.to_string());
        let _ = request.response_tx.send(result);
    }

    /// The editor runs outside the VM; once it finishes, resume precisely the
    /// saved effect rather than resubmitting source or replaying prior output.
    fn spawn_deferred_proposal(
        &self,
        query_id: Uuid,
        round_token: ToolRoundToken,
        tool_id: String,
        proposal: DeferredProposal,
        approval_audience: Option<crate::brain::store::BrainApprovalAudience>,
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
                let decision =
                    crate::tools::implementations::propose::propose_artifact_with_decision(
                        &proposal.language,
                        &intent,
                        &proposal.source,
                    )
                    .await?;
                let outcome =
                    resume_deferred_proposal(runtime.as_ref(), &proposal, decision).await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&outcome)?)
            }
            .await;
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                round_token,
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
        round_token: ToolRoundToken,
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
                    .resolve_typed_approval(&approval.prompt, choice, "interactive-tool-user")
                    .await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&outcome)?)
            }
            .await;
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                round_token,
                tool_id,
                result,
            });
        });
    }

    // ── Diff proposal rendering ───────────────────────────────────────────────

    /// Render a diff proposal visually in the room output.
    ///
    /// ```text
    /// model proposes: src/review/mod.rs
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// - old line
    /// + new line
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// "updates the reviewed changeset"
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

        // Publish the review decision to local proposal consumers.
        let _ = self
            .review_tx
            .send(crate::review::ReviewEvent::diff_reject(diff_id, reason));
        self.render_tui().await
    }

    /// Attach this TUI to one daemon-owned brain. All prompts and explicit
    /// Forth/Lisp programs are routed to that host until `/brain detach`.
    async fn handle_brain_attach(&mut self, value: String) -> Result<()> {
        self.handle_brain_attach_with_invitation(value, None).await
    }

    async fn handle_brain_join(&mut self, value: String, invitation: String) -> Result<()> {
        self.handle_brain_attach_with_invitation(value, Some(invitation))
            .await
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
        self.todo_journal_target.set(self.home_brain.clone());
        if let Some(home) = self.home_brain.as_ref() {
            let snapshot = home.snapshot().await?;
            self.render_remote_brain_message(crate::brain::store::BrainWireMessage::Snapshot {
                brain: snapshot.clone(),
            })
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
            .unwrap_or_else(|| format!("http://{}", crate::config::DEFAULT_HTTP_ADDR));
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

    async fn push_remote_brain(&mut self, kind: crate::brain::store::BrainEventKind) -> Result<()> {
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
            RemoteBrainApprovalKind::Vm { prompt, .. } => vm_approval_dialog(
                prompt,
                Some(&pending.audience),
                self.program_runtime.as_ref(),
            ),
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
        let target = if client.target.secure {
            client.target.display_name()
        } else {
            client.target.brain.clone()
        };
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            format!("◆ {target} · {role}"),
        );
    }

    fn ensure_remote_brain_run_projection(
        &mut self,
        run_id: crate::brain::store::RunId,
        kind: Option<crate::brain::store::BrainRunKind>,
        status: crate::brain::store::BrainRunStatus,
    ) -> &mut RemoteBrainRunProjection {
        ensure_remote_brain_run_projection(
            &self.output_manager,
            &mut self.remote_brain_run_units,
            run_id,
            kind,
            status,
        )
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

    fn conversation_checkpoint_path(&self) -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|home| {
            home.join(".finch")
                .join("sessions")
                .join(format!("{}.json", self.session_uuid))
        })
    }

    /// Persist the latest committed history after each publication boundary.
    /// Staged rounds are omitted by `ConversationHistory` serialization.
    async fn checkpoint_conversation(&self) -> Result<()> {
        let history = self.conversation.read().await;
        self.checkpoint_history(&history)
    }

    fn checkpoint_history(&self, history: &ConversationHistory) -> Result<()> {
        let Some(path) = self.conversation_checkpoint_path() else {
            return Ok(());
        };
        history.save(path)
    }

    fn report_checkpoint_error(&self, context: &str, error: &anyhow::Error) {
        self.output_manager.write_error(format!(
            "{context}: {error:#}. Check free space and write access under ~/.finch/sessions, then retry."
        ));
    }

    /// Close the visible rows owned by a cancelled query without consuming its
    /// detached effect correlation. Named-Brain active tool ids stay pending
    /// until their physical outcomes arrive and #163 records them.
    async fn close_active_tool_rows(&self, query_id: Uuid, reason: &str) {
        let Some(query_unit) = self.query_states.tool_work_unit(query_id).await else {
            return;
        };
        let mut active = self.active_tool_uses.write().await;
        active.retain(|_, (_, _, unit, row_idx)| {
            if Arc::ptr_eq(unit, &query_unit) {
                unit.fail_row(*row_idx, reason);
                false
            } else {
                true
            }
        });
        drop(active);
        self.query_states.set_tool_work_unit(query_id, None).await;
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

        self.tui_renderer.lock().await.poset_panel_mode = crate::cli::tui::PosetPanelMode::Graph;
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
        self.output_manager.write_info(format!(
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
}

fn brain_context_text(
    event: &crate::brain::store::BrainEvent,
    local_machine: Option<&str>,
) -> Option<String> {
    use crate::brain::store::BrainEventKind;

    let text = match &event.kind {
        BrainEventKind::Prompt { text } | BrainEventKind::ParticipantMessage { text } => text,
        BrainEventKind::Result {
            output,
            error: None,
            ..
        } => output,
        _ => return None,
    };
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    let compact = if compact.chars().count() <= 70 {
        compact
    } else {
        format!("{}…", compact.chars().take(69).collect::<String>())
    };
    Some(format!(
        "{}: {compact}",
        participant_display_name(&event.sender, local_machine)
    ))
}

fn project_brain_context(
    status_bar: &crate::cli::status_bar::StatusBar,
    events: &[crate::brain::store::BrainEvent],
    depth: usize,
    local_machine: Option<&str>,
) {
    let lines = projected_brain_context_lines(events, depth, local_machine);
    let count = lines.len();
    for (index, text) in lines.into_iter().enumerate() {
        let label = if count == 1 || index + 1 == count {
            format!("   └─ now: {text}")
        } else if index == 0 {
            format!("💬 {text}")
        } else {
            format!("   ├─ {text}")
        };
        status_bar.update_line(
            crate::cli::status_bar::StatusLineType::BrainContextLine(index),
            label,
        );
    }
    for index in count..8 {
        status_bar.remove_line(&crate::cli::status_bar::StatusLineType::BrainContextLine(
            index,
        ));
    }
}

fn projected_brain_context_lines(
    events: &[crate::brain::store::BrainEvent],
    depth: usize,
    local_machine: Option<&str>,
) -> Vec<String> {
    let speculative_run_ids = events
        .iter()
        .filter_map(|event| match &event.kind {
            crate::brain::store::BrainEventKind::RunStarted { run }
                if run.kind == crate::brain::store::BrainRunKind::Speculative =>
            {
                Some(run.run_id)
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let mut lines = events
        .iter()
        .rev()
        .filter(|event| {
            event
                .run_id
                .is_none_or(|run_id| !speculative_run_ids.contains(&run_id))
        })
        .filter_map(|event| brain_context_text(event, local_machine))
        .take(depth)
        .collect::<Vec<_>>();
    lines.reverse();
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrainRunGroupProjection {
    run_id: crate::brain::store::RunId,
    kind: crate::brain::store::BrainRunKind,
    status: crate::brain::store::BrainRunStatus,
    event_seqs: Vec<u64>,
}

/// Snapshot form of the same RunId grouping used by the live shadow buffer.
/// A snapshot replay and its live tail therefore select one run hierarchy,
/// rather than drawing Program/Result events as unrelated rows.
fn projected_brain_run_groups(
    events: &[crate::brain::store::BrainEvent],
) -> Vec<BrainRunGroupProjection> {
    let mut groups = events
        .iter()
        .filter_map(|event| match &event.kind {
            crate::brain::store::BrainEventKind::RunStarted { run } => {
                Some(BrainRunGroupProjection {
                    run_id: run.run_id,
                    kind: run.kind,
                    status: run.status,
                    event_seqs: Vec::new(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for event in events {
        let Some(run_id) = event.run_id else {
            continue;
        };
        let Some(group) = groups.iter_mut().find(|group| group.run_id == run_id) else {
            continue;
        };
        group.event_seqs.push(event.seq);
        if let crate::brain::store::BrainEventKind::RunStatusChanged { status, .. } = event.kind {
            group.status = status;
        }
    }
    groups
}

fn project_remote_brain_snapshot_runs(
    output_manager: &crate::cli::output_manager::OutputManager,
    projections: &mut std::collections::HashMap<
        crate::brain::store::RunId,
        RemoteBrainRunProjection,
    >,
    local_projections: &mut std::collections::VecDeque<LocalBrainProjection>,
    selected_brain_is_home: bool,
    events: &[crate::brain::store::BrainEvent],
) {
    for group in projected_brain_run_groups(events) {
        ensure_remote_brain_run_projection(
            output_manager,
            projections,
            group.run_id,
            Some(group.kind),
            group.status,
        );
    }
    for event in events.iter().filter(|event| event.run_id.is_some()) {
        project_remote_brain_live_run_event(
            output_manager,
            projections,
            local_projections,
            selected_brain_is_home,
            event,
        );
    }
}

/// Advance the visible projection's canonical cursor. Watch snapshots include
/// every event through their revision, while the live receiver may already
/// have buffered some of those same events. Sequence numbers are authoritative
/// within a Brain, so only a strictly newer event should affect UI chrome.
fn advance_brain_projection_revision(
    revisions: &mut std::collections::HashMap<crate::brain::store::BrainId, u64>,
    brain_id: crate::brain::store::BrainId,
    revision: u64,
) -> bool {
    let projected = revisions.entry(brain_id).or_default();
    if revision <= *projected {
        return false;
    }
    *projected = revision;
    true
}

/// Snapshot replay reconstructs conversation, not transient connection chrome.
/// Presence and runner ownership are projected into the status line from the
/// snapshot itself; replaying their historical transitions pollutes scrollback
/// and can duplicate the first live event delivered after subscription.
fn replay_event_belongs_in_transcript(event: &crate::brain::store::BrainEvent) -> bool {
    use crate::brain::store::BrainEventKind;

    !matches!(
        event.kind,
        BrainEventKind::RunnerLeaseAcquired { .. }
            | BrainEventKind::RunnerLeaseReleased { .. }
            | BrainEventKind::RunnerHandoffRequested { .. }
            | BrainEventKind::RunnerHandoffCompleted { .. }
            | BrainEventKind::RunnerHandoffCancelled { .. }
            | BrainEventKind::ClientAttached { .. }
            | BrainEventKind::ClientDetached { .. }
            | BrainEventKind::RunStarted { .. }
            | BrainEventKind::RunStatusChanged { .. }
    )
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

fn confirmation_audit_value(confirmation: &super::events::ConfirmationResult) -> serde_json::Value {
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
mod tests;

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
