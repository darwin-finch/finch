// Shammah - Local-first Constitutional AI Proxy
// Library exports

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

// Cap'n Proto generated code must live at the crate root so that the
// self-references emitted by capnpc (`crate::finch_ipc_capnp::…`) resolve.
#[allow(
    clippy::all,
    dead_code,
    unused_imports,
    unused_parens,
    non_camel_case_types,
    non_snake_case
)]
pub mod finch_ipc_capnp {
    include!(concat!(env!("OUT_DIR"), "/finch_ipc_capnp.rs"));
}

// Core modules
pub mod agent; // Autonomous agent loop (task backlog, reflection, activity log)
pub mod brain; // Background context-gathering agent (spawned when user starts typing)
pub mod claude;
pub mod cli;
pub mod client; // HTTP client for daemon communication (Phase 8)
pub mod coforth; // Co-Forth English library — every word as a Forth word
pub mod config;
pub mod context; // Project context (CLAUDE.md / FINCH.md auto-loading)
pub mod daemon; // Daemon lifecycle and auto-spawn (Phase 8)
pub mod errors; // User-friendly error messages
pub mod feedback; // Response feedback system for LoRA training
pub mod generators; // Unified generator interface
pub mod graph; // Execution graph — causal trace of query turns
pub mod ipc; // Cap'n Proto IPC layer (CLI ↔ daemon over Unix socket)
pub mod license;
pub mod llms; // Generic LLM abstraction (Phase 1)
pub mod local; // Local generation system
pub mod logging; // Conversation logging for LoRA training
pub mod memory; // Hierarchical memory system (Phase 4)
pub mod metrics;
pub mod models; // Phase 2: Neural network models
pub mod monitoring; // System monitoring (memory, CPU)
pub mod network; // Lotus Network device registration and membership
pub mod node; // Node identity and work statistics (distributed worker)
pub mod node_name; // Per-machine cute name (e.g. "tiny-bird"), persisted to ~/.finch/node_name
pub mod peer_token; // Peer authentication token for daemon endpoints
pub mod planning; // IMPCPD iterative plan refinement loop
pub mod poset; // Co-Forth poset VM — partially-ordered task graph with 3D renderer
pub mod programs; // Persistent shared Forth/Lisp program vocabulary
pub mod providers; // Multi-provider LLM support
pub mod samples;   // Sample spreadsheet generator (finch samples)
pub mod lisp;      // Scheme-flavoured Lisp dialect with async SSH + crypto
pub mod ssh;       // SSH client (russh) — sessions referenced from Lisp
pub mod registry; // Peer registry — machines check in, you query who's alive
pub mod router;
pub mod runtime; // Provider-neutral Forth/Lisp execution and capabilities
pub mod scheduling; // Autonomous task scheduling (Phase 5)
pub mod server; // HTTP daemon mode (Phase 1)
pub mod service; // Service discovery (Phase 3)
pub mod session; // Bidirectional event bus — local or WebSocket transport
pub mod tools; // Tool execution system
pub mod training; // Batch training and checkpoints (Phase 2) // Offline Ed25519 commercial license key validation
