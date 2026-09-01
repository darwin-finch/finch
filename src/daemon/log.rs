//! Bounded, rotating persistent daemon log.
//!
//! The daemon appends diagnostics to `~/.finch/daemon.log` for the life of the
//! installation. Without a retention boundary that file grows without limit; a
//! live dogfood host reached roughly 705 MiB, which dominated the Finch state
//! directory and buried current events under stale traces (issue #240).
//!
//! This module bounds the log by size and file count. Rotation renames the
//! active file rather than copying or truncating it, so it is atomic, safe
//! across daemon restart and crash, and never destroys diagnostics that a
//! concurrent reader or appender still holds open.
//!
//! Redaction is unchanged: this module moves bytes, it never formats events, so
//! whatever secret-redaction guarantees the emitting layer provides are
//! preserved verbatim.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(any(test, unix))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Default size at which the active log rotates.
pub const DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Default number of rotated files retained alongside the active log.
pub const DEFAULT_MAX_FILES: usize = 5;

/// Upper bound on retained generations.
///
/// `rotate_files` performs a stat and a rename per generation while holding
/// the writer mutex, so an unbounded environment value would let
/// `FINCH_DAEMON_LOG_MAX_FILES=100000000` wedge the daemon behind ~10^8
/// syscalls on every rollover.
pub const MAX_RETAINED_FILES: usize = 64;

/// Environment override for [`RotationPolicy::max_bytes`].
pub const ENV_MAX_BYTES: &str = "FINCH_DAEMON_LOG_MAX_BYTES";

/// Environment override for [`RotationPolicy::max_files`].
pub const ENV_MAX_FILES: &str = "FINCH_DAEMON_LOG_MAX_FILES";

/// Owner-only permissions for the daemon log and its rotated generations.
#[cfg(unix)]
const LOG_MODE: u32 = 0o600;

/// Bounded retention policy for the daemon log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// Rotate once the active file would exceed this many bytes.
    pub max_bytes: u64,
    /// Number of rotated generations retained beside the active file.
    pub max_files: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

impl RotationPolicy {
    /// Read the policy from the environment, falling back to bounded defaults.
    ///
    /// An unparseable or zero value is ignored in favour of the default so a
    /// malformed environment can never disable retention.
    pub fn from_env() -> Self {
        let default = Self::default();
        let max_bytes = std::env::var(ENV_MAX_BYTES)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_bytes);
        let max_files = std::env::var(ENV_MAX_FILES)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.min(MAX_RETAINED_FILES))
            .unwrap_or(default.max_files);
        Self {
            max_bytes,
            max_files,
        }
    }

    /// Worst-case disk budget: the active file plus every retained generation.
    ///
    /// This bound assumes no single write exceeds `max_bytes`. A record larger
    /// than the limit is always admitted whole rather than split, so with
    /// oversized records the true bound is
    /// `(max_files + 1) * max(max_bytes, largest_record)`. At the 16 MiB
    /// default that requires a single tracing event above 16 MiB.
    pub fn retention_ceiling_bytes(&self) -> u64 {
        self.max_bytes.saturating_mul(self.max_files as u64 + 1)
    }
}

/// Point-in-time accounting for the daemon log, for status and setup surfaces.
#[derive(Debug, Clone)]
pub struct LogStatus {
    /// Absolute path of the active log file.
    pub path: PathBuf,
    /// Size of the active file in bytes.
    pub active_bytes: u64,
    /// Combined size of retained rotated generations in bytes.
    pub rotated_bytes: u64,
    /// Number of retained rotated generations.
    pub rotated_files: usize,
    /// Policy currently in force.
    pub policy: RotationPolicy,
    /// False when an archived generation may have kept a permissive mode
    /// because it could not be re-secured before its rename.
    pub hardened: bool,
}

impl LogStatus {
    /// Total bytes currently occupied by the active file and its generations.
    pub fn total_bytes(&self) -> u64 {
        self.active_bytes.saturating_add(self.rotated_bytes)
    }

    /// One-line, secret-free summary suitable for status and setup surfaces.
    pub fn summary(&self) -> String {
        let mut summary = format!(
            "{} — {} of {} max ({} kept, rotates at {}); inspect: tail -f {}",
            self.path.display(),
            human_bytes(self.total_bytes()),
            human_bytes(self.policy.retention_ceiling_bytes()),
            self.policy.max_files,
            human_bytes(self.policy.max_bytes),
            self.path.display(),
        );
        if !self.hardened {
            summary.push_str(
                " — WARNING: an archived copy could not be re-secured and may be readable by others",
            );
        }
        summary
    }
}

/// Render a byte count with a stable, locale-free unit.
fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// The canonical daemon log path, `~/.finch/daemon.log`.
pub fn daemon_log_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Failed to determine home directory")?
        .join(".finch")
        .join("daemon.log"))
}

/// Path of rotated generation `index` (1 is the most recent).
fn generation_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

/// Public guard for callers that must open the log without owning rotation,
/// such as the frontend creating the file it hands to the spawned daemon.
pub fn ensure_regular_file(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => bail!(
            "Refusing to use daemon log {}: not a regular file. A FIFO or \
             device node here would block the daemon indefinitely on open",
            path.display()
        ),
        _ => Ok(()),
    }
}

/// Reject any path whose final component is a symlink.
///
/// Rotation renames and removes files. Following a symlink would let a link
/// planted in `~/.finch` redirect those operations at an unrelated file, so the
/// active log and every generation must be a regular file or absent.
fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "Refusing to use daemon log {}: path is a symlink; \
                 remove it or point the daemon at a regular file",
                path.display()
            );
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

/// Open the active log for append with owner-only permissions.
fn open_append(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(LOG_MODE);
        // O_NOFOLLOW closes the window between the symlink check and this
        // open: a path swapped to a symlink in between fails here instead of
        // being followed into an unrelated file.
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("Failed to open daemon log: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An existing file keeps its own mode; restore owner-only explicitly so
        // a previously world-readable log does not stay readable after rotation.
        let mut perms = file
            .metadata()
            .with_context(|| format!("Failed to stat daemon log: {}", path.display()))?
            .permissions();
        if perms.mode() & 0o777 != LOG_MODE {
            perms.set_mode(LOG_MODE);
            // fchmod through the open handle; the path-based form follows
            // symlinks and would retarget the change.
            file.set_permissions(perms)
                .with_context(|| format!("Failed to secure daemon log: {}", path.display()))?;
        }
    }
    Ok(file)
}

/// Restore owner-only permissions on an existing log before it is renamed.
#[cfg(unix)]
fn secure_existing_file(path: &Path) -> Result<bool> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if !path.exists() {
        // Nothing to secure, and nothing was skipped.
        return Ok(true);
    }
    // Securing the archive is best-effort. A log the owner has deliberately
    // made write-only cannot be opened for reading, and refusing to start the
    // daemon over a hardening step the user chose would be worse than leaving
    // the mode alone.
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            // Do not print here. Under the TUI this path is reachable through
            // `ensure_daemon_running` after the subscriber is installed, where a
            // raw write corrupts the frame; and in the detached daemon fd 2 is
            // this very file, which is renamed on the next line, so the message
            // would be filed inside the permissive archive it warns about.
            // The skip is reported through `LogStatus` instead, which the
            // startup line prints wherever that line goes.
            tracing::warn!(
                path = %path.display(),
                %error,
                "Could not open the daemon log to restore owner-only permissions before rotation"
            );
            return Ok(false);
        }
    };
    let mut perms = file
        .metadata()
        .with_context(|| format!("Failed to stat daemon log: {}", path.display()))?
        .permissions();
    if perms.mode() & 0o777 != LOG_MODE {
        perms.set_mode(LOG_MODE);
        file.set_permissions(perms)
            .with_context(|| format!("Failed to secure daemon log: {}", path.display()))?;
    }
    Ok(true)
}

#[cfg(not(unix))]
fn secure_existing_file(_path: &Path) -> Result<bool> {
    Ok(true)
}

/// Size of `path`, or zero when it does not exist.
fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[derive(Debug)]
struct State {
    file: File,
    written: u64,
    /// Set when a reopen after rotation failed, so `file` refers to a rotated
    /// inode rather than the active path. Cleared once a reopen succeeds.
    degraded: bool,
}

/// Test-only injection point for a failing reopen.
///
/// The reopen cannot be made to fail by permissions alone: `rename` and the
/// subsequent create both require write access to the directory, so removing it
/// fails the rename first and the degraded path is never reached. Injecting the
/// failure is the only way to exercise it deterministically.
#[cfg(test)]
fn take_injected_reopen_failure(log: &RotatingLog) -> Option<io::Error> {
    if log.fail_next_reopen.swap(false, Ordering::SeqCst) {
        return Some(io::Error::other("injected reopen failure"));
    }
    None
}

/// An append-only log writer that rotates within a bounded disk budget.
///
/// Cloning yields another handle to the same file and rotation state, which is
/// what `tracing_subscriber`'s `MakeWriter` closure requires.
#[derive(Clone, Debug)]
pub struct RotatingLog {
    path: PathBuf,
    policy: RotationPolicy,
    state: Arc<Mutex<State>>,
    /// False when an archived generation could not be re-secured before its
    /// rename, so it may have kept a more permissive mode.
    hardened: bool,
    /// Whether this process's stdout/stderr are bound to the log and must be
    /// re-pointed at the new file on every rotation. Only unix binds
    /// descriptors, so only unix reads this.
    #[cfg(unix)]
    bind_stdio: Arc<AtomicBool>,
    /// Test-only: forces the next reopen after rotation to fail exactly once.
    #[cfg(test)]
    fail_next_reopen: Arc<AtomicBool>,
}

impl RotatingLog {
    /// Open (or create) the daemon log under `policy`.
    ///
    /// A log that is already over budget — including one inherited from a build
    /// with no retention at all — is rotated rather than truncated or deleted,
    /// so existing diagnostics survive as a generation. Rotation is a rename,
    /// so this stays O(1) regardless of how large the inherited file is and
    /// never blocks startup on copying hundreds of megabytes.
    pub fn open(path: &Path, policy: RotationPolicy) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        reject_symlink(path)?;

        let mut hardened = true;
        if file_size(path) >= policy.max_bytes {
            // Restore owner-only mode *before* the rename. `rename` preserves
            // the mode, so rotating first would archive a log inherited from a
            // build that created it at 0644 while it was still world-readable.
            hardened = secure_existing_file(path)?;
            rotate_files(path, policy)?;
        }
        prune_generations(path, policy)?;

        let file = open_append(path)?;
        let written = file_size(path);
        Ok(Self {
            path: path.to_path_buf(),
            policy,
            state: Arc::new(Mutex::new(State {
                file,
                written,
                degraded: false,
            })),
            hardened,
            #[cfg(unix)]
            bind_stdio: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_next_reopen: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Test-only: force the next reopen after rotation to fail once.
    #[cfg(test)]
    fn fail_next_reopen_once(&self) {
        self.fail_next_reopen.store(true, Ordering::SeqCst);
    }

    /// Absolute path of the active log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Policy currently in force.
    pub fn policy(&self) -> RotationPolicy {
        self.policy
    }

    /// Current on-disk accounting for the active file and its generations.
    pub fn status(&self) -> LogStatus {
        let mut status = log_status(&self.path, self.policy);
        status.hardened = self.hardened;
        status
    }
}

/// On-disk accounting for `path` under `policy`, usable without an open handle.
pub fn log_status(path: &Path, policy: RotationPolicy) -> LogStatus {
    let mut rotated_bytes: u64 = 0;
    let mut rotated_files: usize = 0;
    for index in 1..=policy.max_files {
        let generation = generation_path(path, index);
        if generation.exists() {
            rotated_files += 1;
            rotated_bytes = rotated_bytes.saturating_add(file_size(&generation));
        }
    }
    LogStatus {
        path: path.to_path_buf(),
        active_bytes: file_size(path),
        rotated_bytes,
        rotated_files,
        policy,
        hardened: true,
    }
}

/// Remove generations beyond the retention count, including strays left by a
/// previously larger policy, so the retained file count is exact.
fn prune_generations(path: &Path, policy: RotationPolicy) -> Result<()> {
    let mut index = policy.max_files + 1;
    // Stop after a bounded run of absent indices so a gap left by a partial
    // prior rotation does not hide a stray generation beyond it.
    let mut misses = 0;
    while misses < 16 {
        let generation = generation_path(path, index);
        if generation.exists() {
            reject_symlink(&generation)?;
            std::fs::remove_file(&generation)
                .with_context(|| format!("Failed to remove {}", generation.display()))?;
            misses = 0;
        } else {
            misses += 1;
        }
        index += 1;
    }
    Ok(())
}

/// Shift generations down and move the active file to generation 1.
///
/// Every step is a rename or unlink, so a crash between steps leaves a
/// well-formed prefix that the next `open` repairs: missing indices are simply
/// skipped, and nothing is ever partially written.
fn rotate_files(path: &Path, policy: RotationPolicy) -> Result<()> {
    let oldest = generation_path(path, policy.max_files);
    if oldest.exists() {
        reject_symlink(&oldest)?;
        std::fs::remove_file(&oldest)
            .with_context(|| format!("Failed to remove {}", oldest.display()))?;
    }

    for index in (1..policy.max_files).rev() {
        let source = generation_path(path, index);
        if !source.exists() {
            // A gap from an interrupted prior rotation; nothing to shift.
            continue;
        }
        reject_symlink(&source)?;
        let destination = generation_path(path, index + 1);
        std::fs::rename(&source, &destination).with_context(|| {
            format!(
                "Failed to rotate {} to {}",
                source.display(),
                destination.display()
            )
        })?;
    }

    if path.exists() {
        reject_symlink(path)?;
        let destination = generation_path(path, 1);
        std::fs::rename(path, &destination).with_context(|| {
            format!(
                "Failed to rotate {} to {}",
                path.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

/// Point this process's stdout and stderr at `file`.
///
/// `dup2` replaces the descriptor's target, so writes that never pass through
/// `tracing` — `println!`, panic output, and ONNX Runtime's C++ stderr — land in
/// the current active log rather than in whatever inode the descriptor was
/// pointing at when the process was spawned.
#[cfg(unix)]
fn bind_stdio_to(file: &File) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let source = file.as_raw_fd();
    for target in [nix::libc::STDOUT_FILENO, nix::libc::STDERR_FILENO] {
        loop {
            // SAFETY: `source` is an open descriptor owned by this process and
            // `target` is a standard descriptor number. `dup2` is a plain
            // syscall with no allocation or locking.
            if unsafe { nix::libc::dup2(source, target) } != -1 {
                break;
            }
            let error = io::Error::last_os_error();
            // macOS documents EINTR for dup2; a signal delivered mid-rollover
            // must not abort the bind and leave stdio on the archived inode.
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error)
                .with_context(|| format!("Failed to bind descriptor {target} to the daemon log"));
        }
    }
    Ok(())
}

impl RotatingLog {
    /// Take ownership of this process's stdout and stderr.
    ///
    /// The daemon calls this once at startup. Afterwards every rotation
    /// re-points both descriptors at the new active file, so output that
    /// bypasses `tracing` stays inside the retention boundary instead of
    /// growing an archived generation without limit.
    #[cfg(unix)]
    pub fn bind_process_stdio(&self) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("daemon log mutex poisoned"))?;
        bind_stdio_to(&state.file)?;
        self.bind_stdio.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The guarded state is not corrupt after a panic — at worst `written` is
        // stale, and it is re-synced from disk below — so recover the guard
        // instead of disabling daemon file logging for the process lifetime.
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // A previous rotation could not reopen the active path, so `file` still
        // refers to a rotated inode. Retry before writing anything into it.
        if state.degraded {
            match open_append(&self.path) {
                Ok(file) => {
                    state.file = file;
                    state.written = 0;
                    state.degraded = false;
                    #[cfg(unix)]
                    if self.bind_stdio.load(Ordering::SeqCst) {
                        bind_stdio_to(&state.file).map_err(io::Error::other)?;
                    }
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }

        // Re-sync from disk. Descriptors bound to this file by
        // `bind_process_stdio`, and any other appender, add bytes this counter
        // never sees; a ceiling computed from our own writes alone would not
        // be a ceiling.
        let on_disk = file_size(&self.path);
        if on_disk > state.written {
            state.written = on_disk;
        }

        if state.written > 0
            && state.written.saturating_add(buf.len() as u64) > self.policy.max_bytes
        {
            state.file.flush()?;

            // Clear the counter before any fallible step. If rotation succeeds
            // but reopening fails, leaving the old count would keep this branch
            // true on every later write, rotating repeatedly and destroying
            // every retained generation within `max_files` writes.
            state.written = 0;

            rotate_files(&self.path, self.policy).map_err(io::Error::other)?;

            // If reopening fails, `state.file` still refers to the inode that
            // was just renamed to generation 1. Leaving it installed would send
            // every later write into a generation that shifts down and is
            // eventually unlinked. Mark the writer degraded instead, and retry
            // the open at the top of the next write.
            #[cfg(test)]
            if let Some(injected) = take_injected_reopen_failure(self) {
                state.degraded = true;
                return Err(injected);
            }

            match open_append(&self.path) {
                Ok(file) => {
                    state.file = file;
                    state.degraded = false;
                    // Follow the rename with the process descriptors, so output
                    // that never passes through tracing does not stay pinned to
                    // the archived inode.
                    #[cfg(unix)]
                    if self.bind_stdio.load(Ordering::SeqCst) {
                        bind_stdio_to(&state.file).map_err(io::Error::other)?;
                    }
                }
                Err(error) => {
                    state.degraded = true;
                    return Err(io::Error::other(error));
                }
            }
        }

        let written = state.file.write(buf)?;
        state.written = state.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read(path: &Path) -> String {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn policy(max_bytes: u64, max_files: usize) -> RotationPolicy {
        RotationPolicy {
            max_bytes,
            max_files,
        }
    }

    #[test]
    fn test_daemon_log_recovers_after_a_failed_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let mut log = RotatingLog::open(&path, policy(32, 2)).unwrap();
        log.write_all(b"first-record-padding-padding\n").unwrap();

        // Force the reopen that follows the rename to fail. Permissions cannot
        // do this: `rename` and the create both need write on the directory, so
        // removing it fails the rename first and never reaches this branch.
        log.fail_next_reopen_once();
        let failed = log.write_all(b"second-record-padding-padding\n");
        assert!(failed.is_err(), "the injected reopen failure must surface");

        // The rename already happened, so the handle now refers to generation 1.
        // Writing through it would put post-rotation output into an archive that
        // shifts down and is eventually unlinked.
        assert!(
            generation_path(&path, 1).exists(),
            "rotation completed before the reopen failed"
        );

        log.write_all(b"after-recovery\n").unwrap();
        log.flush().unwrap();

        let active = read(&path);
        assert!(
            active.contains("after-recovery"),
            "the writer must recover onto the active path; got {active:?}"
        );
        let archived = read(&generation_path(&path, 1));
        assert!(
            archived.contains("first-record"),
            "the pre-rotation record belongs in the archive"
        );
        assert!(
            !archived.contains("after-recovery"),
            "a rotated generation must never receive writes made after rotation"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_generation_rotated_by_a_write_is_owner_readable_only() {
        // Characterization, not a regression. `open_append` sets the mode on
        // create, so this has held on every revision of this branch that had it;
        // there is no base-revision control because the module is new. It pins
        // the property against a future change to write-path rotation.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let mut log = RotatingLog::open(&path, policy(32, 2)).unwrap();

        // Rotation triggered from the write path, not from `open`.
        log.write_all(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaa\n").unwrap();
        log.write_all(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbb\n").unwrap();
        log.flush().unwrap();

        let archived = generation_path(&path, 1);
        assert!(archived.exists(), "the write path must have rotated");
        let mode = std::fs::metadata(&archived).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, LOG_MODE,
            "a generation rotated by a write must also be owner-only"
        );
    }

    #[test]
    fn test_daemon_log_rotates_when_size_limit_crossed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let mut log = RotatingLog::open(&path, policy(32, 3)).unwrap();

        log.write_all(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaa\n").unwrap();
        assert!(
            !generation_path(&path, 1).exists(),
            "no rotation under limit"
        );

        log.write_all(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbb\n").unwrap();
        assert!(
            generation_path(&path, 1).exists(),
            "crossing the limit must rotate"
        );
        assert!(read(&generation_path(&path, 1)).contains("aaaa"));
        assert!(read(&path).contains("bbbb"));
    }

    #[test]
    fn test_daemon_log_retains_exact_generation_count_and_bounded_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let p = policy(32, 2);
        let mut log = RotatingLog::open(&path, p).unwrap();

        for _ in 0..20 {
            log.write_all(b"cccccccccccccccccccccccccccc\n").unwrap();
        }

        assert!(
            !generation_path(&path, 3).exists(),
            "retention count is exact"
        );
        let status = log.status();
        assert_eq!(status.rotated_files, 2);
        assert!(
            status.total_bytes() <= p.retention_ceiling_bytes(),
            "total {} exceeded ceiling {}",
            status.total_bytes(),
            p.retention_ceiling_bytes()
        );
    }

    #[test]
    fn test_daemon_log_migrates_oversized_inherited_log_without_data_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, "inherited-705-mib-stand-in\n".repeat(10)).unwrap();

        let log = RotatingLog::open(&path, policy(32, 3)).unwrap();

        assert_eq!(log.status().active_bytes, 0, "active file starts fresh");
        assert!(
            read(&generation_path(&path, 1)).contains("inherited-705-mib-stand-in"),
            "inherited diagnostics must be preserved as a generation, not deleted"
        );
    }

    #[test]
    fn test_daemon_log_reopen_after_restart_preserves_prior_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let p = policy(32, 3);

        let mut first = RotatingLog::open(&path, p).unwrap();
        first.write_all(b"dddddddddddddddddddddddddddd\n").unwrap();
        first.write_all(b"eeeeeeeeeeeeeeeeeeeeeeeeeeee\n").unwrap();
        drop(first);

        let mut second = RotatingLog::open(&path, p).unwrap();
        second.write_all(b"post-restart\n").unwrap();

        // The post-restart write crosses the limit again, so the pre-restart
        // generations shift down rather than staying at fixed indices.
        let retained: String = (1..=3)
            .map(|index| {
                let generation = generation_path(&path, index);
                if generation.exists() {
                    read(&generation)
                } else {
                    String::new()
                }
            })
            .collect();
        assert!(
            retained.contains("dddd"),
            "a generation from before the restart must survive it"
        );
        assert!(
            retained.contains("eeee"),
            "reopening appends to the prior active file, it never truncates it"
        );
        assert!(read(&path).contains("post-restart"));
    }

    #[test]
    fn test_daemon_log_repairs_partial_prior_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        // A crash mid-rotation can leave generation 2 present with 1 absent.
        std::fs::write(&path, "active\n").unwrap();
        std::fs::write(generation_path(&path, 2), "orphan-generation\n").unwrap();

        let mut log = RotatingLog::open(&path, policy(8, 3)).unwrap();
        log.write_all(b"forces-rotation\n").unwrap();

        assert!(
            read(&generation_path(&path, 3)).contains("orphan-generation"),
            "the orphaned generation shifts down instead of blocking rotation"
        );
    }

    #[test]
    fn test_daemon_log_prunes_generations_left_by_a_larger_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        for index in 1..=6 {
            std::fs::write(generation_path(&path, index), "stale\n").unwrap();
        }

        let log = RotatingLog::open(&path, policy(1024, 2)).unwrap();

        assert!(generation_path(&path, 2).exists());
        assert!(!generation_path(&path, 3).exists(), "beyond retention");
        assert!(!generation_path(&path, 6).exists(), "beyond retention");
        assert_eq!(log.status().rotated_files, 2);
    }

    #[test]
    fn test_daemon_log_concurrent_appends_stay_within_retention_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let p = policy(64, 2);
        let log = RotatingLog::open(&path, p).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let mut handle = log.clone();
                std::thread::spawn(move || {
                    for _ in 0..40 {
                        handle.write_all(b"concurrent-append-line\n").unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let status = log.status();
        assert!(
            status.total_bytes() <= p.retention_ceiling_bytes(),
            "concurrent appends exceeded the ceiling: {} > {}",
            status.total_bytes(),
            p.retention_ceiling_bytes()
        );
        assert!(!generation_path(&path, 3).exists());
    }

    #[test]
    #[cfg(unix)]
    fn test_daemon_log_rejects_symlinked_active_path() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("unrelated.txt");
        std::fs::write(&victim, "must-not-be-rotated\n").unwrap();
        let path = dir.path().join("daemon.log");
        std::os::unix::fs::symlink(&victim, &path).unwrap();

        let err = RotatingLog::open(&path, RotationPolicy::default()).unwrap_err();

        assert!(
            err.to_string().contains("symlink"),
            "error must name the symlink refusal: {err}"
        );
        assert_eq!(read(&victim), "must-not-be-rotated\n");
    }

    #[test]
    #[cfg(unix)]
    fn test_daemon_log_is_owner_readable_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, "pre-existing\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // Large enough to force the rotation inside `open`, which is the path an
        // upgrade takes over a log inherited from a build with no retention.
        std::fs::write(&path, "inherited world-readable history\n".repeat(8)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut log = RotatingLog::open(&path, policy(16, 2)).unwrap();
        log.write_all(b"rotate-me-please\n").unwrap();

        let active_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(active_mode, LOG_MODE, "active log must stay owner-only");

        let archived = generation_path(&path, 1);
        assert!(archived.exists(), "the inherited log must be archived");
        let archived_mode = std::fs::metadata(&archived).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            archived_mode, LOG_MODE,
            "rename preserves mode, so the archive must be secured before \
             rotation; otherwise an inherited 0644 log stays world-readable"
        );
    }

    /// Serializes the tests that mutate process-global environment variables.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_rotation_policy_from_env_rejects_malformed_overrides() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let default = RotationPolicy::default();

        for bad in ["0", "-1", "abc", "", "9999999999999999999999"] {
            std::env::set_var(ENV_MAX_BYTES, bad);
            std::env::set_var(ENV_MAX_FILES, bad);
            let policy = RotationPolicy::from_env();
            assert_eq!(
                policy, default,
                "override {bad:?} must fall back to the default rather than \
                 disabling retention"
            );
        }

        std::env::remove_var(ENV_MAX_BYTES);
        std::env::remove_var(ENV_MAX_FILES);
    }

    #[test]
    fn test_rotation_policy_from_env_accepts_and_clamps_valid_overrides() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        std::env::set_var(ENV_MAX_BYTES, "  4096  ");
        std::env::set_var(ENV_MAX_FILES, "3");
        let policy = RotationPolicy::from_env();
        assert_eq!(policy.max_bytes, 4096, "surrounding whitespace is trimmed");
        assert_eq!(policy.max_files, 3);

        // An absurd retention count would make every rotation perform that many
        // syscalls while holding the writer mutex.
        std::env::set_var(ENV_MAX_FILES, "100000000");
        assert_eq!(
            RotationPolicy::from_env().max_files,
            MAX_RETAINED_FILES,
            "retention count must be clamped"
        );

        std::env::remove_var(ENV_MAX_BYTES);
        std::env::remove_var(ENV_MAX_FILES);
    }

    #[test]
    fn test_oversized_record_is_admitted_whole_and_bound_is_documented() {
        // A single record larger than max_bytes cannot be split, so each
        // generation holds one oversized record. The ceiling is therefore
        // stated in terms of the largest record, not max_bytes alone.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let p = policy(32, 2);
        let mut log = RotatingLog::open(&path, p).unwrap();

        let record = vec![b'x'; 40];
        for _ in 0..6 {
            log.write_all(&record).unwrap();
        }
        log.flush().unwrap();

        let status = log.status();
        let honest_bound = (p.max_files as u64 + 1) * record.len().max(32) as u64;
        assert!(
            status.total_bytes() <= honest_bound,
            "total {} exceeded the record-aware bound {}",
            status.total_bytes(),
            honest_bound
        );
        assert!(!generation_path(&path, 3).exists(), "count stays exact");
    }

    #[test]
    fn test_log_status_summary_reports_path_size_and_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, "x".repeat(2048)).unwrap();

        let summary = log_status(&path, policy(4096, 3)).summary();

        assert!(summary.contains("daemon.log"));
        assert!(summary.contains("2.0 KiB"));
        assert!(summary.contains("3 kept"));
        assert!(summary.contains("tail -f"));
    }
}
