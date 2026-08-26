// Feedback system for response quality tracking
//
// Explicit feedback is retained in ~/.finch/feedback.jsonl. Recording feedback
// does not trigger training.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

/// Feedback rating for a response
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    /// Positive feedback (good response)
    Good,
    /// Negative feedback (bad response)
    Bad,
}

impl FeedbackRating {
    /// Get the historical feedback weight.
    pub fn training_weight(&self) -> f64 {
        match self {
            FeedbackRating::Good => 1.0, // Normal weight (1x)
            FeedbackRating::Bad => 10.0, // High weight (10x) - learn from mistakes
        }
    }

    /// Get display string
    pub fn display_str(&self) -> &'static str {
        match self {
            FeedbackRating::Good => "👍 Good",
            FeedbackRating::Bad => "👎 Bad",
        }
    }
}

/// Canonical feedback entry logged to JSONL.
///
/// Deserialization also accepts the legacy daemon `WeightedExample` shape
/// (`query`, `response`, `weight`, `feedback`) so an existing mixed file stays
/// readable without migration.
#[derive(Debug, Clone, Serialize)]
pub struct FeedbackEntry {
    /// Timestamp (Unix timestamp)
    pub timestamp: u64,
    /// User query that generated the response
    pub query: String,
    /// Response that was rated
    pub response: String,
    /// Feedback rating
    pub rating: FeedbackRating,
    /// Training weight (derived from rating)
    pub weight: f64,
    /// Optional note from user
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl<'de> Deserialize<'de> for FeedbackEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct CompatibleFeedbackEntry {
            #[serde(default)]
            timestamp: Option<u64>,
            query: String,
            response: String,
            #[serde(default)]
            rating: Option<FeedbackRating>,
            #[serde(default)]
            weight: Option<f64>,
            #[serde(default)]
            note: Option<String>,
            #[serde(default)]
            feedback: Option<String>,
        }

        let compatible = CompatibleFeedbackEntry::deserialize(deserializer)?;
        let rating = compatible.rating.unwrap_or_else(|| {
            if compatible.weight.unwrap_or(1.0) > 1.0 {
                FeedbackRating::Bad
            } else {
                FeedbackRating::Good
            }
        });

        Ok(Self {
            timestamp: compatible.timestamp.unwrap_or(0),
            query: compatible.query,
            response: compatible.response,
            rating,
            weight: compatible
                .weight
                .unwrap_or_else(|| rating.training_weight()),
            note: compatible.note.or(compatible.feedback),
        })
    }
}

impl FeedbackEntry {
    /// Create a new feedback entry
    pub fn new(query: String, response: String, rating: FeedbackRating) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            query,
            response,
            weight: rating.training_weight(),
            rating,
            note: None,
        }
    }

    /// Create an entry from an explicitly weighted feedback request.
    pub fn weighted(query: String, response: String, weight: f64, note: Option<String>) -> Self {
        let rating = if weight > 1.0 {
            FeedbackRating::Bad
        } else {
            FeedbackRating::Good
        };
        let mut entry = Self::new(query, response, rating);
        entry.weight = weight;
        entry.note = note;
        entry
    }

    /// Add a note to the feedback
    pub fn with_note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }
}

/// Canonical feedback store and JSONL writer.
///
/// Every writer takes an OS-level exclusive lock, appends exactly one canonical
/// record, and syncs it before returning. Readers take a shared lock. A torn
/// final line is retained as evidence, skipped by readers, and separated from
/// the next valid record on append.
#[derive(Debug, Clone)]
pub struct FeedbackLogger {
    file_path: PathBuf,
}

impl FeedbackLogger {
    /// Create a new feedback logger
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

        Self::at(home.join(".finch").join("feedback.jsonl"))
    }

    /// Create a logger at an explicit path. Used by isolated tests and server
    /// fixtures so they never touch the user's Finch state.
    pub fn at(file_path: impl Into<PathBuf>) -> Result<Self> {
        let logger = Self {
            file_path: file_path.into(),
        };
        logger.ensure_private_storage()?;
        Ok(logger)
    }

    /// Log a feedback entry
    pub fn log(&self, entry: &FeedbackEntry) -> Result<()> {
        let mut file = self.open_private(true)?;
        file.lock_exclusive().with_context(|| {
            format!("Failed to lock feedback log: {}", self.file_path.display())
        })?;

        let len = file.metadata()?.len();
        if len > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0_u8; 1];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // Preserve a complete legacy line or torn bytes verbatim, but
                // ensure the next canonical record starts on a fresh line.
                file.write_all(b"\n")?;
            }
        }

        let mut json = serde_json::to_vec(entry).context("Failed to serialize feedback entry")?;
        json.push(b'\n');
        file.write_all(&json)
            .context("Failed to write feedback entry")?;
        file.sync_all().context("Failed to sync feedback entry")?;
        Ok(())
    }

    /// Get the path to the feedback log
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    /// Count total feedback entries
    pub fn count_entries(&self) -> Result<usize> {
        Ok(self.load_all()?.len())
    }

    /// Load all feedback entries
    pub fn load_all(&self) -> Result<Vec<FeedbackEntry>> {
        let file = self.open_private(false)?;
        file.lock_shared().with_context(|| {
            format!("Failed to lock feedback log: {}", self.file_path.display())
        })?;
        let mut entries = Vec::new();
        for line in BufReader::new(&file).split(b'\n') {
            let line = line.context("Failed to read feedback log")?;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<FeedbackEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(error) => tracing::warn!(
                    path = %self.file_path.display(),
                    %error,
                    "Skipping malformed or torn feedback record"
                ),
            }
        }

        Ok(entries)
    }

    fn ensure_private_storage(&self) -> Result<()> {
        let parent = self
            .file_path
            .parent()
            .context("Feedback log path has no parent directory")?;
        let parent_existed = parent.exists();

        if let Ok(metadata) = fs::symlink_metadata(parent) {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Feedback directory must be a real directory: {}",
                parent.display()
            );
        } else {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).with_context(|| {
                format!("Failed to create feedback directory: {}", parent.display())
            })?;
        }

        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "Failed to make feedback directory private: {}",
                parent.display()
            )
        })?;

        if !parent_existed {
            sync_directory(parent.parent().unwrap_or(parent))?;
        }

        if let Ok(metadata) = fs::symlink_metadata(&self.file_path) {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Feedback log must be a regular file: {}",
                self.file_path.display()
            );
        }

        let file_existed = self.file_path.exists();
        let file = self
            .open_file_options(true)
            .open(&self.file_path)
            .with_context(|| {
                format!("Failed to open feedback log: {}", self.file_path.display())
            })?;
        make_file_private(&file, &self.file_path)?;
        file.sync_all()?;
        if !file_existed {
            sync_directory(parent)?;
        }
        Ok(())
    }

    fn open_private(&self, append: bool) -> Result<File> {
        self.ensure_private_storage()?;
        let file = self
            .open_file_options(append)
            .open(&self.file_path)
            .with_context(|| {
                format!("Failed to open feedback log: {}", self.file_path.display())
            })?;
        make_file_private(&file, &self.file_path)?;
        Ok(file)
    }

    fn open_file_options(&self, append: bool) -> OpenOptions {
        let mut options = OpenOptions::new();
        options.read(true);
        if append {
            options.append(true).create(true);
        }
        #[cfg(unix)]
        options.mode(0o600);
        options
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("Failed to open directory for sync: {}", path.display()))?
        .sync_all()
        .with_context(|| format!("Failed to sync directory: {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn make_file_private(file: &File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to make feedback log private: {}", path.display()))?;
    #[cfg(not(unix))]
    let _ = (file, path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_feedback_rating_weights() {
        assert_eq!(FeedbackRating::Good.training_weight(), 1.0);
        assert_eq!(FeedbackRating::Bad.training_weight(), 10.0);
    }

    #[test]
    fn test_feedback_entry_creation() {
        let entry = FeedbackEntry::new(
            "What is 2+2?".to_string(),
            "4".to_string(),
            FeedbackRating::Good,
        );

        assert_eq!(entry.query, "What is 2+2?");
        assert_eq!(entry.response, "4");
        assert_eq!(entry.rating, FeedbackRating::Good);
        assert_eq!(entry.weight, 1.0);
        assert!(entry.note.is_none());
    }

    #[test]
    fn test_feedback_entry_with_note() {
        let entry = FeedbackEntry::new(
            "Test".to_string(),
            "Response".to_string(),
            FeedbackRating::Bad,
        )
        .with_note("Wrong algorithm".to_string());

        assert_eq!(entry.note, Some("Wrong algorithm".to_string()));
    }

    #[test]
    fn mixed_canonical_and_legacy_feedback_stays_readable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".finch/feedback.jsonl");
        fs::create_dir(path.parent().unwrap()).unwrap();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "query": "legacy",
                "response": "old answer",
                "weight": 3.0,
                "feedback": "legacy note"
            })
        )
        .unwrap();
        file.sync_all().unwrap();
        drop(file);

        let logger = FeedbackLogger::at(&path).unwrap();
        logger
            .log(&FeedbackEntry::new(
                "canonical".into(),
                "answer".into(),
                FeedbackRating::Good,
            ))
            .unwrap();

        let entries = logger.load_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].timestamp, 0);
        assert_eq!(entries[0].rating, FeedbackRating::Bad);
        assert_eq!(entries[0].note.as_deref(), Some("legacy note"));
        assert_eq!(entries[1].query, "canonical");
    }

    #[test]
    fn torn_tail_is_retained_and_does_not_block_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".finch/feedback.jsonl");
        let logger = FeedbackLogger::at(&path).unwrap();
        logger
            .log(&FeedbackEntry::new(
                "before".into(),
                "answer".into(),
                FeedbackRating::Good,
            ))
            .unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"timestamp":12,"query":"torn""#)
            .unwrap();
        file.sync_all().unwrap();
        drop(file);

        let restarted = FeedbackLogger::at(&path).unwrap();
        assert_eq!(restarted.load_all().unwrap().len(), 1);
        restarted
            .log(&FeedbackEntry::new(
                "after".into(),
                "answer".into(),
                FeedbackRating::Bad,
            ))
            .unwrap();

        let raw = fs::read(&path).unwrap();
        assert!(raw
            .windows(b"\"query\":\"torn\"".len())
            .any(|window| window == b"\"query\":\"torn\""));
        let entries = restarted.load_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].query, "before");
        assert_eq!(entries[1].query, "after");
    }

    #[cfg(unix)]
    #[test]
    fn feedback_directory_and_file_are_private() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".finch");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let path = directory.join("feedback.jsonl");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

        FeedbackLogger::at(&path).unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn independent_processes_serialize_feedback_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".finch/feedback.jsonl");
        FeedbackLogger::at(&path).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for process in 0..4 {
            children.push(
                Command::new(&executable)
                    .args(["--exact", "feedback::tests::feedback_child_writer"])
                    .env("FINCH_FEEDBACK_CHILD_PATH", &path)
                    .env("FINCH_FEEDBACK_CHILD_ID", process.to_string())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }

        let entries = FeedbackLogger::at(&path).unwrap().load_all().unwrap();
        assert_eq!(entries.len(), 100);
    }

    #[test]
    fn feedback_child_writer() {
        let Ok(path) = std::env::var("FINCH_FEEDBACK_CHILD_PATH") else {
            return;
        };
        let process = std::env::var("FINCH_FEEDBACK_CHILD_ID").unwrap();
        let logger = FeedbackLogger::at(path).unwrap();
        for entry in 0..25 {
            logger
                .log(&FeedbackEntry::new(
                    format!("process-{process}-{entry}"),
                    "answer".into(),
                    FeedbackRating::Good,
                ))
                .unwrap();
        }
    }
}
