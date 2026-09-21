//! The parts an `EventLoop` is built from, grouped by what they are for.
//!
//! `EventLoop::new` took thirty-five parameters. Nobody can hold thirty-five things at once, so a
//! caller does not read such a signature — it copies an existing call and edits what looks wrong,
//! which is how a `_cloud_gen` that nothing uses survives, and how two adjacent `usize` limits get
//! swapped without anyone noticing.
//!
//! These are plain owned structs with public fields: no builders, no defaults, no validation. The
//! grouping is the whole point, and adding anything else would hide it.

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::local::LocalGenerator;
use crate::models::GeneratorState;

/// Who is talking, as what, and in which mode.
pub struct SessionParts {
    pub conversation: Arc<RwLock<ConversationHistory>>,
    pub active_persona: Arc<RwLock<crate::config::Persona>>,
    pub mode: Arc<RwLock<ReplMode>>,
    pub label: String,
}

/// What produces tokens, and which provider is currently chosen.
pub struct GenerationParts {
    pub generator: Arc<dyn crate::generators::Generator>,
    pub router: Arc<crate::router::Router>,
    pub state: Arc<RwLock<GeneratorState>>,
    pub local: Arc<RwLock<LocalGenerator>>,
    pub tokenizer: Arc<crate::models::TextTokenizer>,
    pub resolver: crate::scheduler::ProviderResolver,
    pub available: Vec<crate::config::ProviderEntry>,
    pub active_index: usize,
    pub default_provider: Option<String>,
    pub cli_model: Option<String>,
    pub cli_provider: Option<String>,
}

/// Everything that draws.
pub struct UiParts {
    pub renderer: TuiRenderer,
    pub output: Arc<OutputManager>,
    pub status_bar: Arc<StatusBar>,
    pub streaming_enabled: bool,
    pub mention_port: Arc<dyn crate::cli::tui::MentionPort>,
}

/// Tools the model may call, and the task list it keeps.
pub struct ToolParts {
    pub definitions: Vec<crate::tools::ToolDefinition>,
    pub executor: Arc<Mutex<crate::tools::ToolExecutor>>,
    pub todo_list: Arc<RwLock<crate::tools::TodoList>>,
    pub todo_journal_target: crate::tools::TodoJournalTarget,
    pub todo_journal_receiver: crate::tools::TodoJournalReceiver,
    pub memory_commitment_target: super::memory_commitment::MemoryCommitmentTarget,
    pub memory_commitment_receiver: super::memory_commitment::MemoryCommitmentReceiver,
}

/// How this frontend reaches a daemon, and why it could not.
pub struct DaemonParts {
    pub ipc_client: Option<crate::client::IpcClient>,
    pub ipc_error: Option<String>,
    pub client: Option<Arc<crate::client::DaemonClient>>,
    pub base_url: Option<String>,
}

/// How much history to carry, and when to compact it.
///
/// Four numbers and two flags of the same types, which is exactly the shape that gets transposed
/// at a call site and produces a subtly wrong session nobody can explain.
pub struct ContextLimits {
    pub lines: usize,
    pub max_verbatim_messages: usize,
    pub recall_k: usize,
    pub enable_summarization: bool,
    pub auto_compact: bool,
}

/// What executes typed programs and remembers.
pub struct RuntimeParts {
    pub program_runtime: Arc<crate::runtime::ProgramRuntime>,
    pub agent_scheduler: Arc<crate::scheduler::AgentScheduler>,
    pub memory_system: Option<Arc<finch_memory::MemorySystem>>,
    /// Local mirror of the selected Brain's committed (byte-stable) memory
    /// set (#940), shared with `LlmRuntime`'s copy so both the attach-time
    /// hydration (`EventLoop`) and the per-turn render (`LlmLoop`) see the
    /// same state.
    pub committed_memories:
        Arc<RwLock<Vec<crate::cli::repl_event::memory_commitment::CommittedMemoryRecord>>>,
    /// `EventLoop` does not push through this itself; it only holds it to
    /// hand to each `LlmLoop` it constructs, which is the task that
    /// actually decides and requests committed-set replacements.
    pub memory_commitment_writer: super::memory_commitment::MemoryCommitmentWriter,
}

// ---------------------------------------------------------------------------
// The LLM loop's parts.
//
// `LlmLoop` runs on its own task, so it holds shared handles where `EventLoop` owns values:
// `Arc<RwLock<TuiRenderer>>` rather than a `TuiRenderer`, `Arc<RwLock<Vec<ToolDefinition>>>`
// rather than a `Vec`. The groups mean the same things; only the sharing differs, which is why
// these are separate types rather than a reuse that would force one side into the other's shape.
//
// `ContextLimits` is shared, because those five values are identical on both sides.
// ---------------------------------------------------------------------------

/// Where requests arrive and events are published.
pub struct LlmChannels {
    pub requests: tokio::sync::mpsc::UnboundedReceiver<super::LlmRequest>,
    pub events: tokio::sync::mpsc::UnboundedSender<super::ReplEvent>,
}

/// What produces tokens, and how a request is routed between them.
pub struct LlmGeneration {
    pub cloud: Arc<RwLock<Arc<dyn crate::generators::Generator>>>,
    pub local: Arc<dyn crate::generators::Generator>,
    pub router: Arc<crate::router::Router>,
    pub state: Arc<RwLock<GeneratorState>>,
}

/// Tools the model may call, and what is in flight.
pub struct LlmTools {
    pub definitions: Arc<RwLock<Vec<crate::tools::ToolDefinition>>>,
    pub coordinator: super::tool_execution::ToolExecutionCoordinator,
    pub call_history: super::query_processor::ToolCallHistory,
    pub active_uses: super::query_processor::ActiveToolUsesMap,
}

/// Everything the loop draws through.
pub struct LlmUi {
    pub output: Arc<OutputManager>,
    pub status_bar: Arc<StatusBar>,
    pub renderer: Arc<Mutex<TuiRenderer>>,
    pub streaming_enabled: bool,
}

/// Who is talking, as what, from where.
pub struct LlmSession {
    pub conversation: Arc<RwLock<ConversationHistory>>,
    pub active_persona: Arc<RwLock<crate::config::Persona>>,
    pub mode: Arc<RwLock<ReplMode>>,
    pub query_states: Arc<super::query_state::QueryStateManager>,
    pub label: String,
    pub cwd: String,
}

/// What executes programs, what remembers, and what records.
pub struct LlmRuntime {
    pub program_runtime: Arc<crate::runtime::ProgramRuntime>,
    pub memory_system: Option<Arc<finch_memory::MemorySystem>>,
    pub current_graph: Arc<Mutex<crate::graph::ExecutionGraph>>,
    pub wire_metrics_logger: Option<Arc<crate::metrics::MetricsLogger>>,
    /// Shared with `RuntimeParts`'s copy -- see its doc comment.
    pub committed_memories:
        Arc<RwLock<Vec<crate::cli::repl_event::memory_commitment::CommittedMemoryRecord>>>,
    pub memory_commitment_writer: super::memory_commitment::MemoryCommitmentWriter,
}
