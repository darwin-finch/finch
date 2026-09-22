// Host-facing progress for bootstrap status lines.
// CLI implements this port; models never names CLI types.

/// Host-supplied progress reporting for model bootstrap messages.
///
/// Composition roots inject an implementation; CLI owns the TUI types.
pub trait ModelProgress: Send + Sync {
    /// Write a bootstrap or status line.
    fn write_progress(&self, content: String);
}

/// No-op sink used when no host is attached.
///
/// Composition roots that need bootstrap messages inject a reporting host;
/// compatibility and test loaders may explicitly choose this sink.
pub struct SilentModelProgress;

impl ModelProgress for SilentModelProgress {
    fn write_progress(&self, _content: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct RecordingProgress {
        lines: Mutex<Vec<String>>,
    }

    impl ModelProgress for RecordingProgress {
        fn write_progress(&self, content: String) {
            self.lines.lock().unwrap().push(content);
        }
    }

    #[test]
    fn silent_progress_is_usable_without_cli() {
        let sink: Arc<dyn ModelProgress> = Arc::new(SilentModelProgress);
        sink.write_progress("loading".into());
    }

    #[test]
    fn recording_progress_captures_bootstrap_lines() {
        let sink = RecordingProgress {
            lines: Mutex::new(Vec::new()),
        };
        sink.write_progress("⏳ Loading Qwen...".into());
        assert_eq!(
            sink.lines.lock().unwrap().as_slice(),
            ["⏳ Loading Qwen..."]
        );
    }
}
