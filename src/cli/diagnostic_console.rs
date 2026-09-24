use finch_tui::{DiagnosticConsolePort, DiagnosticConsoleSnapshot};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

const MAX_BYTES_PER_LOG: u64 = 256 * 1024;
const MAX_RETAINED_LINES: usize = 2_000;

static FRONTEND_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Register the diagnostic log owned by this frontend process.
pub fn register_frontend_log_path(path: PathBuf) {
    let _ = FRONTEND_LOG_PATH.set(path);
}

pub(crate) fn source() -> Arc<dyn DiagnosticConsolePort> {
    let mut logs = Vec::new();
    if let Ok(path) = crate::daemon::daemon_log_path() {
        logs.push(("daemon".to_string(), path));
    }
    if let Some(path) = FRONTEND_LOG_PATH.get() {
        logs.push(("frontend".to_string(), path.clone()));
    }
    Arc::new(FileDiagnosticConsole::new(logs))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct State {
    fingerprints: Vec<Option<Fingerprint>>,
    snapshot: DiagnosticConsoleSnapshot,
}

#[derive(Debug)]
struct FileDiagnosticConsole {
    logs: Vec<(String, PathBuf)>,
    state: Mutex<State>,
}

impl FileDiagnosticConsole {
    fn new(logs: Vec<(String, PathBuf)>) -> Self {
        let count = logs.len();
        Self {
            logs,
            state: Mutex::new(State {
                fingerprints: vec![None; count],
                snapshot: DiagnosticConsoleSnapshot::default(),
            }),
        }
    }

    fn fingerprints(&self) -> Vec<Option<Fingerprint>> {
        self.logs
            .iter()
            .map(|(_, path)| {
                let metadata = std::fs::metadata(path).ok()?;
                Some(Fingerprint {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                })
            })
            .collect()
    }

    fn read_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (label, path) in &self.logs {
            let Ok(mut log_lines) = read_tail_lines(path, MAX_BYTES_PER_LOG) else {
                continue;
            };
            if log_lines.is_empty() {
                continue;
            }
            lines.push(finch_diff::sanitize_terminal(&format!(
                "── {label}: {} ──",
                path.display()
            )));
            lines.append(&mut log_lines);
        }
        if lines.len() > MAX_RETAINED_LINES {
            lines.drain(..lines.len() - MAX_RETAINED_LINES);
        }
        lines
    }
}

impl DiagnosticConsolePort for FileDiagnosticConsole {
    fn snapshot(&self) -> DiagnosticConsoleSnapshot {
        let fingerprints = self.fingerprints();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.fingerprints == fingerprints {
            return state.snapshot.clone();
        }
        state.fingerprints = fingerprints;
        state.snapshot.revision = state.snapshot.revision.wrapping_add(1);
        state.snapshot.lines = self.read_lines();
        state.snapshot.clone()
    }
}

fn read_tail_lines(path: &Path, max_bytes: u64) -> std::io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text.as_ref()
    };
    Ok(text.lines().map(finch_diff::sanitize_terminal).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_file_console_refreshes_and_sanitizes_appended_output() {
        let dir = tempfile::tempdir().expect("temporary diagnostic directory");
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, "ready\n").expect("seed daemon log");
        let source = FileDiagnosticConsole::new(vec![("daemon".to_string(), path.clone())]);

        let first = source.snapshot();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open daemon log for append");
        writeln!(file, "\x1b[31mfailed\x1b[0m").expect("append diagnostic");
        file.flush().expect("flush diagnostic append");
        let second = source.snapshot();

        assert_ne!(
            first.revision, second.revision,
            "an appended diagnostic line must advance the console revision"
        );
        assert!(
            second.lines.iter().any(|line| line == "failed"),
            "the console must retain the diagnostic text without terminal controls: {:?}",
            second.lines
        );
        assert!(
            second.lines.iter().all(|line| !line.contains('\x1b')),
            "diagnostic logs must not be able to inject terminal controls: {:?}",
            second.lines
        );
    }
}
