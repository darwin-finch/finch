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

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
compile_error!("secure feedback storage is currently supported only on Linux and macOS");

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
        let file = self.open_private(true)?;
        self.append_to_open_file(file, entry)
    }

    fn append_to_open_file(&self, mut file: File, entry: &FeedbackEntry) -> Result<()> {
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
        let parent_existed = parent.try_exists().with_context(|| {
            format!("Failed to inspect feedback directory: {}", parent.display())
        })?;
        if !parent_existed {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).with_context(|| {
                format!("Failed to create feedback directory: {}", parent.display())
            })?;
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let parent_directory = open_private_directory(parent)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let parent_directory = File::open(parent)
            .with_context(|| format!("Failed to open feedback directory: {}", parent.display()))?;

        make_object_private(&parent_directory, parent, 0o700)?;

        if !parent_existed {
            sync_directory(parent.parent().unwrap_or(parent))?;
        }

        let (file, file_created) = self.open_from(&parent_directory, OpenPurpose::Initialize)?;
        validate_feedback_file(&file, &self.file_path)?;
        make_object_private(&file, &self.file_path, 0o600)?;
        file.sync_all()?;
        if file_created {
            parent_directory.sync_all().with_context(|| {
                format!("Failed to sync feedback directory: {}", parent.display())
            })?;
        }
        Ok(())
    }

    fn open_private(&self, append: bool) -> Result<File> {
        self.open_private_with_parent_hook(append, || {})
    }

    fn open_private_with_parent_hook(
        &self,
        append: bool,
        after_parent_opened: impl FnOnce(),
    ) -> Result<File> {
        self.ensure_private_storage()?;
        let parent = self
            .file_path
            .parent()
            .context("Feedback log path has no parent directory")?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let parent_directory = open_private_directory(parent)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let parent_directory = File::open(parent)
            .with_context(|| format!("Failed to open feedback directory: {}", parent.display()))?;
        after_parent_opened();
        let purpose = if append {
            OpenPurpose::Append
        } else {
            OpenPurpose::Read
        };
        let (file, _) = self.open_from(&parent_directory, purpose)?;
        validate_feedback_file(&file, &self.file_path)?;
        make_object_private(&file, &self.file_path, 0o600)?;
        Ok(file)
    }

    #[cfg(test)]
    fn log_with_parent_hook(
        &self,
        entry: &FeedbackEntry,
        after_parent_opened: impl FnOnce(),
    ) -> Result<()> {
        let file = self.open_private_with_parent_hook(true, after_parent_opened)?;
        self.append_to_open_file(file, entry)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn open_from(&self, _parent: &File, purpose: OpenPurpose) -> Result<(File, bool)> {
        let mut options = OpenOptions::new();
        options.read(true);
        if purpose == OpenPurpose::Append {
            options.append(true);
        } else if purpose == OpenPurpose::Initialize {
            options.write(true).create(true);
        }
        let existed = self.file_path.try_exists()?;
        let file = options.open(&self.file_path).with_context(|| {
            format!("Failed to open feedback log: {}", self.file_path.display())
        })?;
        Ok((file, !existed))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn open_from(&self, parent: &File, purpose: OpenPurpose) -> Result<(File, bool)> {
        let name = self
            .file_path
            .file_name()
            .context("Feedback log path has no file name")?;
        open_feedback_at(parent, name, purpose)
            .with_context(|| format!("Failed to open feedback log: {}", self.file_path.display()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenPurpose {
    Initialize,
    Read,
    Append,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_open {
    #[cfg(target_os = "linux")]
    use std::os::raw::c_uint;
    use std::os::raw::{c_char, c_int};

    #[cfg(target_os = "linux")]
    pub(super) type OpenMode = c_uint;
    // Darwin mode_t is u16, which C's default argument promotions pass as int.
    #[cfg(target_os = "macos")]
    pub(super) type OpenMode = c_int;

    pub(super) const O_RDONLY: c_int = 0;
    pub(super) const O_RDWR: c_int = 2;

    #[cfg(target_os = "linux")]
    pub(super) const O_APPEND: c_int = 0o2000;
    #[cfg(target_os = "macos")]
    pub(super) const O_APPEND: c_int = 0x0008;
    #[cfg(target_os = "linux")]
    pub(super) const O_CREAT: c_int = 0o100;
    #[cfg(target_os = "macos")]
    pub(super) const O_CREAT: c_int = 0x0200;
    #[cfg(target_os = "linux")]
    pub(super) const O_EXCL: c_int = 0o200;
    #[cfg(target_os = "macos")]
    pub(super) const O_EXCL: c_int = 0x0800;
    #[cfg(target_os = "linux")]
    pub(super) const O_DIRECTORY: c_int = 0o200000;
    #[cfg(target_os = "macos")]
    pub(super) const O_DIRECTORY: c_int = 0x100000;
    #[cfg(target_os = "linux")]
    pub(super) const O_NOFOLLOW: c_int = 0o400000;
    #[cfg(target_os = "macos")]
    pub(super) const O_NOFOLLOW: c_int = 0x0100;
    #[cfg(target_os = "linux")]
    pub(super) const O_CLOEXEC: c_int = 0o2000000;
    #[cfg(target_os = "macos")]
    pub(super) const O_CLOEXEC: c_int = 0x1000000;
    #[cfg(target_os = "linux")]
    pub(super) const O_NONBLOCK: c_int = 0o4000;
    #[cfg(target_os = "macos")]
    pub(super) const O_NONBLOCK: c_int = 0x0004;

    unsafe extern "C" {
        pub(super) fn openat(directory: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_private_directory(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(unix_open::O_DIRECTORY | unix_open::O_NOFOLLOW | unix_open::O_CLOEXEC);
    let directory = options.open(path).with_context(|| {
        format!(
            "Feedback directory must be a real directory: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        directory.metadata()?.is_dir(),
        "Feedback directory must be a real directory: {}",
        path.display()
    );
    Ok(directory)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_feedback_at(
    parent: &File,
    name: &std::ffi::OsStr,
    purpose: OpenPurpose,
) -> Result<(File, bool)> {
    let name = CString::new(name.as_bytes()).context("Feedback log name contains a NUL byte")?;
    let common = unix_open::O_NOFOLLOW | unix_open::O_CLOEXEC | unix_open::O_NONBLOCK;
    let open = |flags: std::os::raw::c_int, mode: unix_open::OpenMode| {
        // SAFETY: `name` is NUL-terminated, `parent` remains open for the call,
        // and a successful descriptor is immediately owned by `File`.
        let descriptor =
            unsafe { unix_open::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor as RawFd) })
    };

    if purpose == OpenPurpose::Initialize {
        match open(
            unix_open::O_RDONLY | common | unix_open::O_CREAT | unix_open::O_EXCL,
            0o600,
        ) {
            Ok(file) => return Ok((file, true)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }

    let access = if purpose == OpenPurpose::Append {
        unix_open::O_RDWR | unix_open::O_APPEND
    } else {
        unix_open::O_RDONLY
    };
    Ok((open(access | common, 0)?, false))
}

fn validate_feedback_file(file: &File, path: &Path) -> Result<()> {
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file(),
        "Feedback log must be a regular file: {}",
        path.display()
    );
    #[cfg(unix)]
    anyhow::ensure!(
        metadata.nlink() == 1,
        "Feedback log must not have multiple hard links: {}",
        path.display()
    );
    Ok(())
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

fn make_object_private(file: &File, path: &Path, maximum_mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let current = file.metadata()?.permissions().mode() & 0o777;
        let tightened = current & maximum_mode;
        if tightened != current {
            file.set_permissions(fs::Permissions::from_mode(tightened))
                .with_context(|| {
                    format!(
                        "Failed to make feedback storage private: {}",
                        path.display()
                    )
                })?;
        }
    }
    #[cfg(not(unix))]
    let _ = (file, path, maximum_mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::os::unix::fs::symlink;
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
    fn test_feedback_storage_tightens_unsafe_permissions() {
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

    #[cfg(unix)]
    #[test]
    fn test_private_storage_never_broadens_owner_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".finch");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("feedback.jsonl");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();

        FeedbackLogger::at(&path).unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o500
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o400
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_log_rejects_symlinks_and_hard_links() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".finch");
        fs::create_dir(&directory).unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, b"unchanged").unwrap();

        let symlink_path = directory.join("feedback.jsonl");
        symlink(&victim, &symlink_path).unwrap();
        assert!(FeedbackLogger::at(&symlink_path).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"unchanged");

        fs::remove_file(&symlink_path).unwrap();
        fs::hard_link(&victim, &symlink_path).unwrap();
        let error = FeedbackLogger::at(&symlink_path).unwrap_err();
        assert!(error.to_string().contains("multiple hard links"));
        assert_eq!(fs::read(&victim).unwrap(), b"unchanged");

        let real_directory = temp.path().join("real-finch");
        fs::create_dir(&real_directory).unwrap();
        let linked_directory = temp.path().join("linked-finch");
        symlink(&real_directory, &linked_directory).unwrap();
        assert!(FeedbackLogger::at(linked_directory.join("feedback.jsonl")).is_err());
        assert!(!real_directory.join("feedback.jsonl").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_log_rejects_directories_and_devices() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".finch");
        fs::create_dir(&directory).unwrap();
        let directory_target = directory.join("feedback.jsonl");
        fs::create_dir(&directory_target).unwrap();
        let error = FeedbackLogger::at(&directory_target).unwrap_err();
        assert!(error.to_string().contains("regular file"));

        let dev = File::open("/dev").unwrap();
        let (device, _) =
            open_feedback_at(&dev, std::ffi::OsStr::new("null"), OpenPurpose::Read).unwrap();
        let error = validate_feedback_file(&device, Path::new("/dev/null")).unwrap_err();
        assert!(error.to_string().contains("regular file"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_log_stays_bound_to_validated_parent_during_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".finch");
        let path = directory.join("feedback.jsonl");
        let logger = FeedbackLogger::at(&path).unwrap();
        let moved = temp.path().join("original-finch");
        let victim = temp.path().join("victim");
        fs::write(&victim, b"victim").unwrap();
        let entry = FeedbackEntry::new(
            "descriptor-bound".into(),
            "response".into(),
            FeedbackRating::Good,
        );

        logger
            .log_with_parent_hook(&entry, || {
                fs::rename(&directory, &moved).unwrap();
                fs::create_dir(&directory).unwrap();
                symlink(&victim, directory.join("feedback.jsonl")).unwrap();
            })
            .unwrap();

        let stored = fs::read_to_string(moved.join("feedback.jsonl")).unwrap();
        assert!(stored.contains("\"query\":\"descriptor-bound\""));
        assert_eq!(fs::read(&victim).unwrap(), b"victim");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_replacement_after_logger_creation_cannot_redirect_feedback() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".finch/feedback.jsonl");
        let logger = FeedbackLogger::at(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, b"victim").unwrap();
        symlink(&victim, &path).unwrap();

        assert!(logger
            .log(&FeedbackEntry::new(
                "query".into(),
                "response".into(),
                FeedbackRating::Good,
            ))
            .is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"victim");
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
