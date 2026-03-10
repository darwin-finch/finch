// Shammah - Local-first Constitutional AI Proxy
// Library exports

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
pub mod config;
pub mod context; // Project context (CLAUDE.md / FINCH.md auto-loading)
pub mod daemon; // Daemon lifecycle and auto-spawn (Phase 8)
pub mod errors; // User-friendly error messages
pub mod feedback; // Response feedback system for LoRA training
pub mod generators; // Unified generator interface
pub mod graph; // Execution graph — causal trace of query turns
pub mod ipc;        // Cap'n Proto IPC layer (CLI ↔ daemon over Unix socket)
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
pub mod planning; // IMPCPD iterative plan refinement loop
pub mod coforth;  // Co-Forth English library — every word as a Forth word
pub mod poset;    // Co-Forth poset VM — partially-ordered task graph with 3D renderer
pub mod providers; // Multi-provider LLM support
pub mod peer_token; // Peer authentication token for daemon endpoints
pub mod registry; // Peer registry — machines check in, you query who's alive
pub mod router;
pub mod scheduling; // Autonomous task scheduling (Phase 5)
pub mod server; // HTTP daemon mode (Phase 1)
pub mod service; // Service discovery (Phase 3)
pub mod tools; // Tool execution system
pub mod training; // Batch training and checkpoints (Phase 2) // Offline Ed25519 commercial license key validation
