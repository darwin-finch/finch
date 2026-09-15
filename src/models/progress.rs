// Host-facing progress for bootstrap status lines and determinate downloads.
// CLI implements this port; models never names CLI types.

use std::sync::{Arc, RwLock};

/// Determinate download progress handle returned by [`ModelProgress`].
pub trait DownloadProgressDisplay: Send + Sync {
    /// Update the current progress value.
    fn update(&self, current: u64);
    /// Mark the download complete.
    fn complete(&self);
    /// Mark the download failed.
    fn fail(&self);
}

/// Host-supplied progress reporting for model bootstrap and download.
///
/// Composition roots inject an implementation; CLI owns the TUI types.
pub trait ModelProgress: Send + Sync {
    /// Write a bootstrap or status line.
    fn write_progress(&self, content: String);

    /// Start a determinate download progress item.
    fn start_download_progress(
        &self,
        label: String,
        total: u64,
    ) -> Arc<dyn DownloadProgressDisplay>;
}

/// No-op sink used when no host is attached (daemon, tests, unattended download).
pub struct SilentModelProgress;

struct SilentDownloadProgress;

impl DownloadProgressDisplay for SilentDownloadProgress {
    fn update(&self, _current: u64) {}
    fn complete(&self) {}
    fn fail(&self) {}
}

impl ModelProgress for SilentModelProgress {
    fn write_progress(&self, _content: String) {}

    fn start_download_progress(
        &self,
        _label: String,
        _total: u64,
    ) -> Arc<dyn DownloadProgressDisplay> {
        Arc::new(SilentDownloadProgress)
    }
}

static INSTALLED_PROGRESS: RwLock<Option<Arc<dyn ModelProgress>>> = RwLock::new(None);

/// Install the process-wide download progress sink at a composition root.
///
/// Model download runs inside the loader stack, so CLI injects the sink here
/// rather than threading TUI types through every loader constructor.
pub fn install_model_progress(progress: Arc<dyn ModelProgress>) {
    *INSTALLED_PROGRESS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(progress);
}

/// Progress sink for download, falling back to [`SilentModelProgress`].
pub(crate) fn download_progress_sink() -> Arc<dyn ModelProgress> {
    INSTALLED_PROGRESS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| Arc::new(SilentModelProgress))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingProgress {
        lines: Mutex<Vec<String>>,
        downloads: Mutex<Vec<String>>,
    }

    struct RecordingDownload {
        label: String,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl DownloadProgressDisplay for RecordingDownload {
        fn update(&self, current: u64) {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:{current}", self.label));
        }
        fn complete(&self) {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:complete", self.label));
        }
        fn fail(&self) {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:fail", self.label));
        }
    }

    impl ModelProgress for RecordingProgress {
        fn write_progress(&self, content: String) {
            self.lines.lock().unwrap().push(content);
        }

        fn start_download_progress(
            &self,
            label: String,
            _total: u64,
        ) -> Arc<dyn DownloadProgressDisplay> {
            self.downloads.lock().unwrap().push(label.clone());
            Arc::new(RecordingDownload {
                label,
                events: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    #[test]
    fn silent_progress_is_usable_without_cli() {
        let sink: Arc<dyn ModelProgress> = Arc::new(SilentModelProgress);
        sink.write_progress("loading".into());
        let handle = sink.start_download_progress("Downloading foo".into(), 100);
        handle.update(50);
        handle.complete();
        handle.fail();
    }

    #[test]
    fn recording_progress_captures_bootstrap_lines() {
        let sink = RecordingProgress {
            lines: Mutex::new(Vec::new()),
            downloads: Mutex::new(Vec::new()),
        };
        sink.write_progress("⏳ Loading Qwen...".into());
        assert_eq!(
            sink.lines.lock().unwrap().as_slice(),
            ["⏳ Loading Qwen..."]
        );
    }
}
