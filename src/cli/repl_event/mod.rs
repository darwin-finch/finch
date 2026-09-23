//! Concurrent REPL event-loop infrastructure.
//!
//! This module contains all the machinery that runs the interactive REPL:
//! input handling, query dispatch, tool execution, streaming output, and TUI
//! rendering.  Each submodule has a well-defined responsibility:
//!
//! | Module            | Responsibility |
//! |-------------------|----------------|
//! | [`event_loop`]    | Main `EventLoop` struct; orchestrates everything |
//! | [`events`]        | `ReplEvent` enum — the message bus between tasks |
//! | [`plan_handler`]  | Tool handlers for `PresentPlan` / `AskUserQuestion` / mode gates |
//! | [`query_state`]   | Per-query metadata and state machine |
//! | [`tool_display`]  | Display/formatting helpers for tool output in the TUI |
//! | [`tool_execution`]| `ToolExecutionCoordinator` — concurrent, approval-gated tool dispatch |
//!
//! ## Architecture
//!
//! The event loop runs as a Tokio `select!` over three streams:
//!
//! 1. **User input** (`spawn_input_task`) — keystrokes, submit, Ctrl+C.
//! 2. **Query/tool events** (`ReplEvent` channel) — streaming chunks, tool
//!    results, approval requests.
//! 3. **Render tick** (~100ms) — flushes buffered output to the TUI.
//!
//! Tool calls are dispatched concurrently by `ToolExecutionCoordinator`.
//! Each tool runs in its own Tokio task and sends its result back as a
//! `ReplEvent::ToolResult` message.  The event loop collects all results for
//! a query and sends the next LLM turn once every pending tool has resolved.

mod activity_view;
mod brain_selection;
mod changeset;
mod event_loop;
mod events;
mod llm_loop;
mod memory_commitment;
mod model_selection;
mod parts;
mod plan_handler;
mod query_processor;
mod query_state;
mod recall_gate;
mod runner_recovery;
mod tool_display;
mod tool_execution;

// `EventLoop::new` takes the construction parts, so callers must be able to name them.
pub(crate) use brain_selection::{parse_reasoning_effort, persistable_selection};
pub use brain_selection::{
    resolve_selection, EffectiveSelection, SelectionRequest, SelectionSource,
};
pub(crate) use event_loop::resolve_provider_profile;
pub use event_loop::EventLoop;
pub(crate) use events::LlmRequest;
pub use events::{ConfirmationResult, ReplEvent};
pub use memory_commitment::{
    memory_commitment_journal, CommittedMemoryRecord, MemoryCommitmentReceiver,
    MemoryCommitmentTarget, MemoryCommitmentWriter,
};
pub use parts::{
    ContextLimits, DaemonParts, GenerationParts, RuntimeParts, SessionParts, ToolParts, UiParts,
};
pub(crate) use plan_handler::{
    is_tool_allowed_in_mode, PLANNING_ALLOWED_TOOLS, PLANNING_ALLOWED_TOOL_ALIASES,
};
pub use tool_display::{format_token_count, format_tool_label};
