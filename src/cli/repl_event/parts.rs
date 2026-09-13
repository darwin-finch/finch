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
use uuid::Uuid;

use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::local::LocalGenerator;
use crate::models::bootstrap::GeneratorState;

/// Who is talking, as what, and in which mode.
pub struct SessionParts {
    pub conversation: Arc<RwLock<ConversationHistory>>,
    pub active_persona: Arc<RwLock<crate::config::Persona>>,
    pub mode: Arc<RwLock<ReplMode>>,
    pub label: String,
    pub uuid: Uuid,
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
}

/// Everything that draws.
pub struct UiParts {
    pub renderer: TuiRenderer,
    pub output: Arc<OutputManager>,
    pub status_bar: Arc<StatusBar>,
    pub streaming_enabled: bool,
}

/// Tools the model may call, and the task list it keeps.
pub struct ToolParts {
    pub definitions: Vec<crate::tools::types::ToolDefinition>,
    pub executor: Arc<Mutex<crate::tools::ToolExecutor>>,
    pub todo_list: Arc<RwLock<crate::tools::todo::TodoList>>,
    pub todo_journal_target: crate::tools::todo::TodoJournalTarget,
    pub todo_journal_receiver: crate::tools::todo::TodoJournalReceiver,
}

/// How this frontend reaches a daemon, and why it could not.
pub struct DaemonParts {
    pub ipc_client: Option<crate::ipc::IpcClient>,
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
    pub memory_system: Option<Arc<crate::memory::MemorySystem>>,
}
