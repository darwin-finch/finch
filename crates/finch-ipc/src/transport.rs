//! Unix socket path helpers and accept loop.

use std::path::PathBuf;

/// Default path for the IPC Unix domain socket.
pub const DAEMON_SOCK_PATH: &str = "~/.finch/daemon.sock";

/// Expand `~/` prefix in a socket path.
pub fn sock_path() -> PathBuf {
    let raw = DAEMON_SOCK_PATH;
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}
