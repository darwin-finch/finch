// Daemon module for background HTTP server mode
//
// This module provides daemon lifecycle management, auto-spawn capabilities,
// and utilities for running Shammah as a persistent background service.

mod lifecycle;
mod log;
mod spawn;
mod upgrade;

/// Set by `spawn_daemon` on the detached child. Only a process carrying it
/// takes over its own stdout and stderr; the documented foreground modes
/// (`finch daemon` in a terminal, `finch worker`, the shipped systemd unit)
/// keep writing to the terminal or the journal.
pub const DETACHED_DAEMON_ENV: &str = "FINCH_DAEMON_DETACHED";

pub use self::log::{
    daemon_log_path, frontend_log_dir, frontend_log_identity, frontend_log_path, log_status,
    prune_frontend_logs, LogStatus, RotatingLog, RotationPolicy, DEFAULT_MAX_FRONTEND_LOG_FILES,
};
pub use lifecycle::{DaemonInstanceGuard, DaemonLifecycle, DaemonStopOutcome};
pub use spawn::{ensure_daemon_running, spawn_daemon};
pub use upgrade::{DaemonUpgradePlan, VerifiedDaemonUpgrade};

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn daemon_facade_keeps_child_modules_private() {
        let facade = include_str!("mod.rs");
        let published = facade
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//"))
            .filter(|line| line.starts_with("pub mod "))
            .collect::<Vec<_>>();
        assert!(
            published.is_empty(),
            "daemon facade must keep child modules private; found: {published:?}"
        );
    }

    #[test]
    fn daemon_callers_use_facade_not_child_modules() {
        let children = ["lifecycle", "log", "spawn", "upgrade"];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let daemon = root.join("src/daemon");
        let mut hits = Vec::new();
        for tree in ["src", "tests"] {
            collect_daemon_child_imports(&root.join(tree), &root, &daemon, &children, &mut hits);
        }
        assert!(
            hits.is_empty(),
            "callers outside src/daemon must use crate::daemon::Item, not child modules; found: {hits:?}"
        );
    }

    fn collect_daemon_child_imports(
        dir: &Path,
        root: &Path,
        daemon: &Path,
        children: &[&str],
        hits: &mut Vec<String>,
    ) {
        if dir.starts_with(daemon) {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                hits.push(format!("failed to read {}: {error}", dir.display()));
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_daemon_child_imports(&path, root, daemon, children, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                hits.push(format!("failed to read {}", path.display()));
                continue;
            };
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for child in children {
                    for prefix in ["crate::daemon::", "finch::daemon::"] {
                        let needle = [prefix, child, "::"].concat();
                        if line.contains(&needle) {
                            let rel = path.strip_prefix(root).unwrap_or(&path);
                            hits.push(format!("{}:{}", rel.display(), index + 1));
                        }
                    }
                }
            }
        }
    }
}
