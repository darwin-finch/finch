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

pub mod event_loop;
pub mod events;
pub mod plan_handler;
pub mod query_processor;
pub mod query_state;
pub mod tool_display;
pub mod tool_execution;

pub use event_loop::EventLoop;
pub use events::{ConfirmationResult, ReplEvent};
pub(crate) use query_processor::apply_sliding_window;
pub use query_state::{QueryMetadata, QueryState, QueryStateManager};
pub use tool_execution::ToolExecutionCoordinator;
