// finch - terminal coding assistant
// Library exports

/// One-line description of what Finch is, shown by `finch --help`.
pub const ABOUT: &str = "Terminal coding assistant with typed programs, named Brains, and tool use";

use std::sync::atomic::{AtomicBool, Ordering};

/// Set to `true` when the TUI event loop is active.
///
/// `propose_in_editor` uses this to know it must suspend/resume the TUI
/// instead of just checking `stdin().is_terminal()`.
pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set to `true` while an external editor is open.
///
/// The TUI render loop checks this flag and skips its render pass while set,
/// preventing crossterm writes from clobbering the editor's output.
pub static EDITOR_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark the TUI as active (called at the start of the TUI event loop).
pub fn set_tui_active(active: bool) {
    TUI_ACTIVE.store(active, Ordering::Relaxed);
}

/// Returns `true` when the TUI event loop currently owns the terminal.
pub fn is_tui_active() -> bool {
    TUI_ACTIVE.load(Ordering::Relaxed)
}

/// Set to `true` when the TUI needs a full redraw after returning from an
/// external editor (mirrors `TuiRenderer::resume` which sets `active_rows = 0`).
pub static NEEDS_TUI_REBUILD: AtomicBool = AtomicBool::new(false);

/// Gates the render loop: set `true` before opening an external editor,
/// `false` after it returns.
pub fn set_editor_active(active: bool) {
    EDITOR_ACTIVE.store(active, Ordering::SeqCst);
}

/// Returns `true` while an external editor has the terminal.
pub fn is_editor_active() -> bool {
    EDITOR_ACTIVE.load(Ordering::SeqCst)
}

/// Signal that the TUI needs a full redraw (called when editor closes).
pub fn request_tui_rebuild() {
    NEEDS_TUI_REBUILD.store(true, Ordering::SeqCst);
}

/// Consume the rebuild request; returns `true` if a full redraw is needed.
pub fn take_tui_rebuild() -> bool {
    NEEDS_TUI_REBUILD.swap(false, Ordering::SeqCst)
}

pub use finch_ipc::finch_ipc_capnp;

// Core modules
pub mod agent; // Autonomous agent loop (task backlog, reflection, activity log)
pub use finch_brain as brain; // Durable named-Brain state, credentials, and client transports
#[cfg(test)]
mod brain_application_tests;
pub mod claude;
pub mod cli;
pub mod client; // HTTP client for daemon communication (Phase 8)
pub mod config;
pub mod context; // Project context (CLAUDE.md / FINCH.md auto-loading)
pub mod daemon; // Daemon lifecycle and auto-spawn (Phase 8)
pub mod errors; // User-friendly error messages
pub mod feedback; // Response feedback system for LoRA training
pub mod generators; // Unified generator interface
pub mod graph; // Execution graph — causal trace of query turns
pub use finch_ipc as ipc; // Cap'n Proto IPC schema/protocol/socket core
pub mod license;
/// Compatibility paths for Finch Lisp syntax values and reader functions.
pub mod lisp {
    pub use finch_language::Val;

    /// Compatibility namespace for Finch Lisp reader functions and spanned values.
    pub mod reader {
        pub use finch_language::{parse_math, parse_str, parse_str_spanned, SpannedVal};
    }

    /// Compatibility namespace for Finch Lisp syntax values.
    pub mod types {
        pub use finch_language::Val;
    }
}
pub mod llms; // Generic LLM abstraction (Phase 1)
pub mod local; // Local generation system
pub mod logging; // Conversation logging for LoRA training
pub mod metrics;
pub mod models; // Phase 2: Neural network models
pub mod monitoring; // System monitoring (memory, CPU)
pub mod network; // Lotus Network device registration and membership
pub mod node; // Node identity and work statistics (distributed worker)
pub mod node_name; // Per-machine cute name (e.g. "tiny-bird"), persisted to ~/.finch/node_name
pub mod oauth; // Provider-neutral OAuth 2 authorization and credential lifecycle
pub mod planning; // IMPCPD iterative plan refinement loop
pub mod poset; // Co-Forth poset VM — partially-ordered task graph with 3D renderer
pub mod program_registry; // Caller-owned adapter from programs onto memory's index
pub mod providers; // Multi-provider LLM support
pub mod review; // Local reviewed-changeset projection
pub mod router;
pub use finch_runtime as runtime; // Provider-neutral Forth/Lisp execution and capabilities
pub mod samples; // Sample spreadsheet generator (finch samples)
pub mod scheduler; // Child-agent orchestration: chooses providers and models, runs tasks
pub mod server; // HTTP daemon mode (Phase 1)
pub mod service; // Service discovery (Phase 3)
pub mod startup; // Startup phase timing: #364, instrument and reduce
                 // interactive TUI time-to-ready
pub mod theme; // Colour scheme and semantic bands: what a renderer needs, with no config format
pub mod tools; // Tool execution system
pub mod training; // Batch training and checkpoints (Phase 2) // Offline Ed25519 commercial license key validation
pub use finch_language as language; // Source compilation facade; returns ModuleVerified
pub use finch_ui_model as ui_model; // Terminal-independent application UI identity, widget data, and layout
pub use finch_vm as vm; // Typed stack IR, verifier, capabilities, and language contracts
pub(crate) mod workbook; // Compatibility facade over runtime-owned worksheet bounds (#282)
