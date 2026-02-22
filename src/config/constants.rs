// Project-wide constants
//
// Centralised here so port numbers and other magic values have one
// source of truth. Import via `use crate::config::constants::*;`.

/// Default bind address for the finch daemon.
///
/// Note: Ollama uses 11434 — we use 11435 to avoid conflicts.
/// Tracked in GitHub issue #10.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:11435";

/// Default daemon port number (split from ADDR for contexts that need just the port).
pub const DEFAULT_DAEMON_PORT: u16 = 11435;

/// Default maximum tokens for teacher API requests.
pub const DEFAULT_MAX_TOKENS: u32 = 8000;
