// Feedback system for response quality tracking
//
// Explicit feedback is retained in ~/.finch/feedback.jsonl. Recording feedback
// does not trigger training.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{CString, OsStr};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

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
    #[cfg(test)]
    injected_log_error: Option<String>,
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
        let file_path = file_path.into();
        let file_path = if file_path.is_absolute() {
            file_path
        } else {
            std::env::current_dir()
                .context("Failed to resolve private feedback storage root")?
                .join(file_path)
        };
        let logger = Self {
            file_path,
            #[cfg(test)]
            injected_log_error: None,
        };
        logger.ensure_private_storage()?;
        Ok(logger)
    }

    #[cfg(test)]
    pub(crate) fn with_injected_log_error(mut self, error: impl Into<String>) -> Self {
        self.injected_log_error = Some(error.into());
        self
    }

    /// Log a feedback entry
    pub fn log(&self, entry: &FeedbackEntry) -> Result<()> {
        #[cfg(test)]
        if let Some(error) = &self.injected_log_error {
            anyhow::bail!(error.clone());
        }
        let file = self.open_private(true)?;
        self.append_to_open_file(file, entry, || {})
    }

    fn append_to_open_file(
        &self,
        mut file: File,
        entry: &FeedbackEntry,
        after_lock: impl FnOnce(),
    ) -> Result<()> {
        file.lock_exclusive()
            .context("Failed to lock private feedback log")?;
        after_lock();
        validate_feedback_file_for_use(&file)?;

        let len = file.metadata()?.len();
        if len > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0_u8; 1];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // Preserve a complete legacy line or torn bytes verbatim, but
                // ensure the next canonical record starts on a fresh line.
                validate_feedback_file_for_use(&file)?;
                file.write_all(b"\n")?;
            }
        }

        let mut json = serde_json::to_vec(entry).context("Failed to serialize feedback entry")?;
        json.push(b'\n');
        validate_feedback_file_for_use(&file)?;
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
        file.lock_shared()
            .context("Failed to lock private feedback log")?;
        validate_feedback_file_for_use(&file)?;
        let mut entries = Vec::new();
        for line in BufReader::new(&file).split(b'\n') {
            let line = line.context("Failed to read feedback log")?;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<FeedbackEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(error) => tracing::warn!(
                    %error,
                    "Skipping malformed or torn feedback record"
                ),
            }
        }

        Ok(entries)
    }

    fn ensure_private_storage(&self) -> Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let parent_directory = open_or_create_storage_parent(&self.file_path)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let parent_directory = open_fallback_storage_parent(&self.file_path)?;

        validate_storage_directory(&parent_directory)?;
        make_object_private(&parent_directory, "private feedback directory", 0o700)?;
        validate_storage_directory_for_use(&parent_directory)?;

        let (file, file_created) = self.open_from(&parent_directory, OpenPurpose::Initialize)?;
        validate_feedback_file(&file)?;
        make_object_private(&file, "private feedback log", 0o600)?;
        validate_feedback_file_for_use(&file)?;
        file.sync_all()?;
        if file_created {
            parent_directory
                .sync_all()
                .context("Failed to sync private feedback directory")?;
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let parent_directory = open_or_create_storage_parent(&self.file_path)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let parent_directory = open_fallback_storage_parent(&self.file_path)?;
        validate_storage_directory_for_use(&parent_directory)?;
        after_parent_opened();
        let purpose = if append {
            OpenPurpose::Append
        } else {
            OpenPurpose::Read
        };
        let (file, _) = self.open_from(&parent_directory, purpose)?;
        validate_feedback_file_for_use(&file)?;
        Ok(file)
    }

    #[cfg(test)]
    fn log_with_parent_hook(
        &self,
        entry: &FeedbackEntry,
        after_parent_opened: impl FnOnce(),
    ) -> Result<()> {
        let file = self.open_private_with_parent_hook(true, after_parent_opened)?;
        self.append_to_open_file(file, entry, || {})
    }

    #[cfg(test)]
    fn log_with_lock_hook(&self, entry: &FeedbackEntry, after_lock: impl FnOnce()) -> Result<()> {
        let file = self.open_private(true)?;
        self.append_to_open_file(file, entry, after_lock)
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
        let file = options
            .open(&self.file_path)
            .context("Failed to open private feedback log")?;
        Ok((file, !existed))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn open_from(&self, parent: &File, purpose: OpenPurpose) -> Result<(File, bool)> {
        let name = self
            .file_path
            .file_name()
            .context("Private feedback log has no file name")?;
        open_feedback_at(parent, name, purpose).context("Failed to open private feedback log")
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
    #[cfg(target_os = "macos")]
    use std::os::raw::c_ushort;
    use std::os::raw::{c_char, c_int, c_uint as uid_t};

    #[cfg(target_os = "linux")]
    pub(super) type OpenMode = c_uint;
    // Darwin mode_t is u16, which C's default argument promotions pass as int.
    #[cfg(target_os = "macos")]
    pub(super) type OpenMode = c_int;
    #[cfg(target_os = "linux")]
    pub(super) type RawMode = c_uint;
    #[cfg(target_os = "macos")]
    pub(super) type RawMode = c_ushort;

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
        pub(super) fn mkdirat(directory: c_int, path: *const c_char, mode: RawMode) -> c_int;
        pub(super) fn geteuid() -> uid_t;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_or_create_storage_parent(file_path: &Path) -> Result<File> {
    let parent = file_path
        .parent()
        .context("Private feedback log has no parent directory")?;
    let (trusted_root, relative_parent) = trusted_storage_root(parent)?;
    anyhow::ensure!(
        !relative_parent.as_os_str().is_empty(),
        "Private feedback storage must be below its trusted root"
    );

    let mut directory = open_trusted_root(&trusted_root)?;
    validate_traversal_directory(&directory)?;
    let components: Vec<_> = relative_parent.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("Private feedback storage contains an unsafe path component");
        };
        let next = match open_directory_at(&directory, name) {
            Ok(next) => next,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_directory_at(&directory, name)?;
                directory
                    .sync_all()
                    .context("Failed to sync private feedback directory parent")?;
                open_directory_at(&directory, name)
                    .context("Failed to open newly created private feedback directory component")?
            }
            Err(error) => {
                return Err(error).context("Failed to open private feedback directory component")
            }
        };
        if index + 1 == components.len() {
            validate_storage_directory(&next)?;
        } else {
            validate_traversal_directory(&next)?;
        }
        directory = next;
    }
    Ok(directory)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn trusted_storage_root(parent: &Path) -> Result<(PathBuf, PathBuf)> {
    anyhow::ensure!(
        parent.is_absolute(),
        "Private feedback storage path must be absolute"
    );

    // Walk from the filesystem root so HOME, TMPDIR, and every user-controlled
    // ancestor receive the same descriptor-bound checks. Darwin exposes /var,
    // /tmp, and /etc as platform-managed symlinks into /private; translate
    // those documented aliases lexically before traversal rather than
    // resolving an attacker-replaceable path with canonicalize().
    #[cfg(target_os = "macos")]
    let parent = if let Ok(relative) = parent.strip_prefix("/var") {
        Path::new("/private/var").join(relative)
    } else if let Ok(relative) = parent.strip_prefix("/tmp") {
        Path::new("/private/tmp").join(relative)
    } else if let Ok(relative) = parent.strip_prefix("/etc") {
        Path::new("/private/etc").join(relative)
    } else {
        parent.to_path_buf()
    };
    #[cfg(not(target_os = "macos"))]
    let parent = parent.to_path_buf();

    let relative = parent
        .strip_prefix(Path::new("/"))
        .context("Failed to resolve private feedback storage root")?;
    Ok((PathBuf::from("/"), relative.to_path_buf()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_fallback_storage_parent(file_path: &Path) -> Result<File> {
    let parent = file_path
        .parent()
        .context("Private feedback log has no parent directory")?;
    fs::create_dir_all(parent).context("Failed to create private feedback directory")?;
    File::open(parent).context("Failed to open private feedback directory")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_trusted_root(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(unix_open::O_DIRECTORY | unix_open::O_NOFOLLOW | unix_open::O_CLOEXEC);
    options
        .open(path)
        .context("Failed to open trusted private feedback storage root")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory_at(parent: &File, name: &OsStr) -> std::io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private feedback directory name contains a NUL byte",
        )
    })?;
    let flags =
        unix_open::O_RDONLY | unix_open::O_DIRECTORY | unix_open::O_NOFOLLOW | unix_open::O_CLOEXEC;
    // SAFETY: name is NUL-terminated and parent stays open for the call. The
    // mode argument is ignored because O_CREAT is absent.
    let descriptor = unsafe { unix_open::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor as RawFd) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_directory_at(parent: &File, name: &OsStr) -> Result<()> {
    let name = CString::new(name.as_bytes())
        .context("Private feedback directory name contains a NUL byte")?;
    // SAFETY: name is NUL-terminated, parent remains open, and 0700 is valid
    // for each platform's fixed (non-variadic) mode_t.
    let result = unsafe {
        unix_open::mkdirat(
            parent.as_raw_fd(),
            name.as_ptr(),
            0o700 as unix_open::RawMode,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return Ok(());
    }
    Err(error).context("Failed to create private feedback directory component")
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

fn effective_uid() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // SAFETY: geteuid has no arguments and no failure mode.
        unsafe { unix_open::geteuid() as u32 }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

#[cfg(unix)]
fn validate_trusted_owner_and_mode(owner: u32, mode: u32) -> Result<()> {
    let current = effective_uid();
    anyhow::ensure!(
        owner == current || owner == 0,
        "Private feedback storage ancestor has unsafe ownership"
    );
    let writable_by_others = mode & 0o022 != 0;
    let protected_root_directory = owner == 0 && mode & 0o1000 != 0;
    anyhow::ensure!(
        !writable_by_others || protected_root_directory,
        "Private feedback storage ancestor has unsafe permissions"
    );
    Ok(())
}

fn validate_directory_type_and_links(directory: &File) -> Result<std::fs::Metadata> {
    let metadata = directory.metadata()?;
    anyhow::ensure!(
        metadata.is_dir(),
        "Private feedback storage ancestor is not a directory"
    );
    #[cfg(unix)]
    anyhow::ensure!(
        metadata.nlink() > 0,
        "Private feedback storage ancestor has invalid link metadata"
    );
    Ok(metadata)
}

fn validate_traversal_directory(directory: &File) -> Result<()> {
    let metadata = validate_directory_type_and_links(directory)?;
    #[cfg(unix)]
    validate_trusted_owner_and_mode(metadata.uid(), metadata.mode())?;
    Ok(())
}

fn validate_storage_directory(directory: &File) -> Result<()> {
    let metadata = validate_directory_type_and_links(directory)?;
    #[cfg(unix)]
    anyhow::ensure!(
        metadata.uid() == effective_uid(),
        "Private feedback directory has unsafe ownership"
    );
    Ok(())
}

fn validate_storage_directory_for_use(directory: &File) -> Result<()> {
    validate_storage_directory(directory)?;
    #[cfg(unix)]
    anyhow::ensure!(
        directory.metadata()?.mode() & 0o077 == 0,
        "Private feedback directory has unsafe permissions"
    );
    Ok(())
}

fn validate_feedback_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file(),
        "Private feedback log is not a regular file"
    );
    #[cfg(unix)]
    {
        anyhow::ensure!(
            metadata.uid() == effective_uid(),
            "Private feedback log has unsafe ownership"
        );
        anyhow::ensure!(
            metadata.nlink() == 1,
            "Private feedback log must not have multiple hard links"
        );
    }
    Ok(())
}

fn validate_feedback_file_for_use(file: &File) -> Result<()> {
    validate_feedback_file(file)?;
    #[cfg(unix)]
    anyhow::ensure!(
        file.metadata()?.mode() & 0o077 == 0,
        "Private feedback log has unsafe permissions"
    );
    Ok(())
}

fn make_object_private(file: &File, label: &'static str, maximum_mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let current = file.metadata()?.permissions().mode() & 0o777;
        let tightened = current & maximum_mode;
        if tightened != current {
            file.set_permissions(fs::Permissions::from_mode(tightened))
                .with_context(|| format!("Failed to make {label} private"))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (file, label, maximum_mode);
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
        let error = validate_feedback_file(&device).unwrap_err();
        assert!(error.to_string().contains("regular file"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_log_rejects_ancestor_symlink_substitution() {
        let temp = tempfile::tempdir().unwrap();
        let real_ancestor = temp.path().join("real-ancestor");
        fs::create_dir(&real_ancestor).unwrap();
        let linked_ancestor = temp.path().join("linked-ancestor");
        symlink(&real_ancestor, &linked_ancestor).unwrap();
        let path = linked_ancestor.join("nested/feedback.jsonl");

        assert!(FeedbackLogger::at(&path).is_err());
        assert!(!real_ancestor.join("nested").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_feedback_storage_rejects_injected_untrusted_ownership() {
        let current = effective_uid();
        let untrusted = if current == 1 { 2 } else { 1 };
        let error = validate_trusted_owner_and_mode(untrusted, 0o700).unwrap_err();
        assert!(error.to_string().contains("unsafe ownership"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_log_revalidates_hardlinks_after_lock_before_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".finch/feedback.jsonl");
        let alias = temp.path().join("feedback-alias.jsonl");
        let logger = FeedbackLogger::at(&path).unwrap();
        let entry = FeedbackEntry::new(
            "must-not-write".into(),
            "response".into(),
            FeedbackRating::Good,
        );

        let error = logger
            .log_with_lock_hook(&entry, || fs::hard_link(&path, &alias).unwrap())
            .unwrap_err();

        assert!(error.to_string().contains("multiple hard links"));
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(fs::metadata(&alias).unwrap().len(), 0);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_feedback_storage_errors_redact_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        let sensitive = temp.path().join("customer-secret-project");
        let target = temp.path().join("safe-target");
        fs::create_dir(&target).unwrap();
        symlink(&target, &sensitive).unwrap();
        let path = sensitive.join("feedback.jsonl");

        let error = FeedbackLogger::at(&path).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(!rendered.contains("customer-secret-project"));
        assert!(!rendered.contains(temp.path().to_string_lossy().as_ref()));
        assert!(rendered.contains("private feedback"));
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
