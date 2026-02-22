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

/// Default port for the HTTP daemon / worker server.
///
/// Used by `finch daemon` and `finch worker`.  Port 8000 is the conventional
/// HTTP development port; distinct from the auto-spawned background daemon
/// (port 11435).
pub const DEFAULT_HTTP_PORT: u16 = 8000;

/// Default bind address for the HTTP daemon (localhost only).
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8000";

/// Default bind address for the network worker (all interfaces).
pub const DEFAULT_WORKER_ADDR: &str = "0.0.0.0:8000";
