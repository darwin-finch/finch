//! Test-only Finch process and filesystem isolation supervisor.

#![cfg(unix)]

use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use nix::libc;

/// Teardown bounds, in seconds, and the fixture pause that races them.
///
/// These are ownership bounds, not convenience timeouts. The supervisor must
/// prove the group quiescent before HOME cleanup, so they stay finite; they
/// are generous because a loaded host delays the *child's* exit, never the
/// supervisor's obligation. Under #328 the previous two-second stages expired
/// on a busy developer machine while the supervised process was still on its
/// way to the expected state, which reads as an authority regression.
///
/// The pause below is derived, not chosen. `run_child_stubborn_probe` parks
/// and asserts the supervisor kills it first, so the fixture is only
/// meaningful while its pause exceeds `TEARDOWN_BOUND_SECS`. Written as two
/// independent literals -- which is how they were -- raising the teardown
/// bound for load tolerance silently inverts the ordering and turns a real
/// teardown regression into a passing test.
const TEARDOWN_SIGTERM_SECS: u64 = 4;
const TEARDOWN_SIGKILL_SECS: u64 = 4;
const TEARDOWN_BOUND_SECS: u64 = TEARDOWN_SIGTERM_SECS + TEARDOWN_SIGKILL_SECS;
const STUBBORN_FIXTURE_PAUSE_SECS: u64 = TEARDOWN_BOUND_SECS * 2;

const TEARDOWN_SIGTERM_BOUND: Duration = Duration::from_secs(TEARDOWN_SIGTERM_SECS);
const TEARDOWN_SIGKILL_BOUND: Duration = Duration::from_secs(TEARDOWN_SIGKILL_SECS);
const STUBBORN_FIXTURE_PAUSE: Duration = Duration::from_secs(STUBBORN_FIXTURE_PAUSE_SECS);

/// How long a probe waits for its continuation before giving up.
const PROBE_CONTINUATION_BOUND: Duration = Duration::from_secs(TEARDOWN_BOUND_SECS);

/// Poll spacing for every bounded wait in this binary.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Waits for `condition` to hold, or until `bound` elapses.
///
/// On expiry the error names the phase, what was being awaited, how long it
/// waited against what bound, and whatever `context` reports about the
/// supervised process. The failures this replaces surfaced as bare assertions
/// carrying only a line number, which cannot distinguish a slow host from a
/// lifecycle regression -- the distinction the reader actually needs.
///
/// `context` is only called on the failure path, so it may be expensive, and
/// it must report identities and paths rather than proof material.
fn await_bounded(
    phase: &str,
    awaited: &str,
    bound: Duration,
    mut condition: impl FnMut() -> anyhow::Result<bool>,
    context: impl FnOnce() -> String,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let deadline = started + bound;
    loop {
        if condition()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "{phase}: timed out awaiting {awaited} after {:?} against a {:?} bound; {}",
                started.elapsed(),
                bound,
                context()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

unsafe fn errno_location() -> *mut libc::c_int {
    #[cfg(target_os = "macos")]
    {
        libc::__error()
    }
    #[cfg(target_os = "linux")]
    {
        libc::__errno_location()
    }
}

extern "C" fn record_signal(signal: libc::c_int) {
    let saved_errno = unsafe { *errno_location() };
    PENDING_SIGNAL
        .compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed)
        .ok();
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = signal as u8;
        loop {
            let result = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
            if result == 1 {
                break;
            }
            let error = unsafe { *errno_location() };
            if error == libc::EINTR {
                continue;
            }
            if error == libc::EAGAIN || error == libc::EWOULDBLOCK {
                break;
            }
            break;
        }
    }
    unsafe {
        *errno_location() = saved_errno;
    }
}

fn install_signal_handlers(write_fd: RawFd) -> io::Result<()> {
    SIGNAL_PIPE.store(write_fd, Ordering::Relaxed);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = record_signal as usize;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

fn canonical_directory(label: &str, path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(path.is_absolute(), "{label} must be absolute");
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "{label} must be a non-symlink directory"
    );
    let canonical = path.canonicalize()?;
    anyhow::ensure!(
        canonical.as_os_str() == path.as_os_str(),
        "{label} must be canonical"
    );
    Ok(canonical)
}

fn unsigned_identity(device: u64, inode: u64) -> String {
    format!("{device}:{inode}")
}

fn resolve_real_store(home: &Path) -> anyhow::Result<PathBuf> {
    let finch = home.join(".finch");
    if let Ok(metadata) = fs::symlink_metadata(&finch) {
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "real Finch path must be a non-symlink directory"
        );
    }
    let store = finch.join("brains");
    if let Ok(metadata) = fs::symlink_metadata(&store) {
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "real Brain store must be a non-symlink directory"
        );
    }
    Ok(store)
}

fn hash_manifest_stat(digest: &mut Sha256, relative: &Path, stat: &libc::stat) {
    digest.update(relative.as_os_str().as_encoded_bytes());
    digest.update(stat.st_mode.to_ne_bytes());
    digest.update(stat.st_uid.to_ne_bytes());
    digest.update(stat.st_gid.to_ne_bytes());
    digest.update(stat.st_nlink.to_ne_bytes());
    digest.update(stat.st_dev.to_ne_bytes());
    digest.update(stat.st_ino.to_ne_bytes());
    digest.update(stat.st_size.to_ne_bytes());
}

fn hash_node_identity_times(digest: &mut Sha256, stat: &libc::stat) {
    digest.update(stat.st_mtime.to_ne_bytes());
    digest.update(stat.st_ctime.to_ne_bytes());
    digest.update(stat.st_mtime_nsec.to_ne_bytes());
    digest.update(stat.st_ctime_nsec.to_ne_bytes());
}

fn hash_manifest_directory(
    directory: &mut nix::dir::Dir,
    relative: &Path,
    digest: &mut Sha256,
    nodes: &mut usize,
    bytes: &mut u64,
    name_bytes: &mut usize,
    depth: usize,
    control_root: Option<&Path>,
) -> anyhow::Result<()> {
    use nix::fcntl::{openat, readlinkat, AtFlags, OFlag};
    use nix::sys::stat::{fstat, fstatat, Mode, SFlag};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;

    const MAX_MANIFEST_NODES: usize = 100_000;
    const MAX_MANIFEST_BYTES: u64 = 1 << 30;
    const MAX_MANIFEST_NAME_BYTES: usize = 16 * 1024 * 1024;
    const MAX_MANIFEST_DEPTH: usize = 128;

    anyhow::ensure!(
        depth <= MAX_MANIFEST_DEPTH,
        "real Brain store manifest exceeds its depth bound"
    );

    let mut names = directory
        .iter()
        .map(|entry| {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                return Ok(None);
            }
            *nodes += 1;
            anyhow::ensure!(
                *nodes <= MAX_MANIFEST_NODES,
                "real Brain store manifest exceeds its node bound"
            );
            *name_bytes = name_bytes
                .checked_add(bytes.len())
                .context("real Brain store manifest name count overflowed")?;
            anyhow::ensure!(
                *name_bytes <= MAX_MANIFEST_NAME_BYTES,
                "real Brain store manifest exceeds its filename bound"
            );
            Ok(Some(std::ffi::OsString::from_vec(bytes.to_vec())))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();

    for name in names {
        let child_relative = relative.join(&name);
        let observed = fstatat(
            Some(directory.as_raw_fd()),
            name.as_os_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?;
        hash_manifest_stat(digest, &child_relative, &observed);
        if std::env::var_os("FINCH_TEST_MANIFEST_RACE_NAME").as_deref() == Some(name.as_os_str()) {
            let control_root =
                control_root.context("manifest race probe requires a private root")?;
            let ready = control_root.join(".manifest-race-ready");
            let continuation = control_root.join(".manifest-race-continue");
            fs::write(&ready, b"ready\n")?;
            await_bounded(
                "manifest race probe",
                "the harness to publish its continuation file",
                PROBE_CONTINUATION_BOUND,
                || Ok(Path::new(&continuation).exists()),
                || {
                    format!(
                        "continuation path {}, readiness path {} (written: {})",
                        continuation.display(),
                        ready.display(),
                        ready.exists()
                    )
                },
            )?;
        }
        let kind = SFlag::from_bits_truncate(observed.st_mode);
        if kind.contains(SFlag::S_IFLNK) {
            digest.update(b"link");
            digest.update(
                readlinkat(Some(directory.as_raw_fd()), name.as_os_str())?.as_encoded_bytes(),
            );
        } else if kind.contains(SFlag::S_IFREG) {
            digest.update(b"file");
            let fd = openat(
                Some(directory.as_raw_fd()),
                name.as_os_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            let mut file = unsafe { File::from_raw_fd(fd) };
            let opened = fstat(file.as_raw_fd())?;
            anyhow::ensure!(
                SFlag::from_bits_truncate(opened.st_mode).contains(SFlag::S_IFREG)
                    && opened.st_dev == observed.st_dev
                    && opened.st_ino == observed.st_ino,
                "real Brain store entry changed while opening"
            );
            let length = u64::try_from(opened.st_size)?;
            *bytes = bytes
                .checked_add(length)
                .context("real Brain store manifest byte count overflowed")?;
            anyhow::ensure!(
                *bytes <= MAX_MANIFEST_BYTES,
                "real Brain store manifest exceeds its byte bound"
            );
            let mut remaining = length;
            let mut buffer = [0_u8; 8192];
            while remaining != 0 {
                let chunk = buffer.len().min(remaining as usize);
                let count = file.read(&mut buffer[..chunk])?;
                anyhow::ensure!(count != 0, "real Brain store file changed while hashing");
                digest.update(&buffer[..count]);
                remaining -= count as u64;
            }
        } else if kind.contains(SFlag::S_IFDIR) {
            digest.update(b"dir");
            let mut child = nix::dir::Dir::openat(
                Some(directory.as_raw_fd()),
                name.as_os_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            let opened = fstat(child.as_raw_fd())?;
            anyhow::ensure!(
                opened.st_dev == observed.st_dev && opened.st_ino == observed.st_ino,
                "real Brain store directory changed while opening"
            );
            hash_manifest_directory(
                &mut child,
                &child_relative,
                digest,
                nodes,
                bytes,
                name_bytes,
                depth + 1,
                control_root,
            )?;
        } else if kind.contains(SFlag::S_IFSOCK) {
            digest.update(b"socket");
        } else if kind.contains(SFlag::S_IFIFO) {
            digest.update(b"fifo");
        } else if kind.contains(SFlag::S_IFBLK) {
            digest.update(b"block");
        } else if kind.contains(SFlag::S_IFCHR) {
            digest.update(b"char");
        } else {
            digest.update(b"other");
        }
    }
    Ok(())
}

fn manifest_digest(store: &Path, control_root: Option<&Path>) -> anyhow::Result<String> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::{fstat, Mode, SFlag};

    let fd = match open(
        store,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(nix::errno::Errno::ENOENT) => return Ok(hex::encode(Sha256::digest(b"missing"))),
        Err(error) => return Err(error.into()),
    };
    let root = unsafe { File::from_raw_fd(fd) };
    let root_stat = fstat(root.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(root_stat.st_mode).contains(SFlag::S_IFDIR),
        "real Brain store must remain a directory"
    );
    let mut digest = Sha256::new();
    hash_manifest_stat(&mut digest, Path::new("."), &root_stat);
    digest.update(b"dir");
    let mut directory = nix::dir::Dir::from(root)?;
    let mut nodes = 0;
    let mut bytes = 0;
    let mut name_bytes = 0;
    hash_manifest_directory(
        &mut directory,
        Path::new("."),
        &mut digest,
        &mut nodes,
        &mut bytes,
        &mut name_bytes,
        0,
        control_root,
    )?;
    Ok(hex::encode(digest.finalize()))
}

struct RealNodeIdentityGuard {
    home: File,
    home_device: libc::dev_t,
    home_inode: libc::ino_t,
    finch: Option<File>,
    finch_identity: Option<(libc::dev_t, libc::ino_t)>,
}

impl RealNodeIdentityGuard {
    fn pin(home_path: &Path) -> anyhow::Result<Self> {
        use nix::fcntl::{open, openat, OFlag};
        use nix::sys::stat::{fstat, Mode};
        let home_fd = open(
            home_path,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let home = unsafe { File::from_raw_fd(home_fd) };
        let home_stat = fstat(home.as_raw_fd())?;
        let finch = match openat(
            Some(home.as_raw_fd()),
            ".finch",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Some(unsafe { File::from_raw_fd(fd) }),
            Err(nix::errno::Errno::ENOENT) => None,
            Err(error) => return Err(error.into()),
        };
        let finch_identity = finch
            .as_ref()
            .map(|directory| fstat(directory.as_raw_fd()))
            .transpose()?
            .map(|stat| (stat.st_dev, stat.st_ino));
        Ok(Self {
            home,
            home_device: home_stat.st_dev,
            home_inode: home_stat.st_ino,
            finch,
            finch_identity,
        })
    }

    fn verify_pathnames(&self, home_path: &Path) -> anyhow::Result<()> {
        use nix::fcntl::{open, AtFlags, OFlag};
        use nix::sys::stat::{fstat, fstatat, Mode};
        let reopened_fd = open(
            home_path,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let reopened = unsafe { File::from_raw_fd(reopened_fd) };
        let home_stat = fstat(reopened.as_raw_fd())?;
        anyhow::ensure!(
            home_stat.st_dev == self.home_device && home_stat.st_ino == self.home_inode,
            "real HOME identity changed"
        );
        let observed_finch = fstatat(
            Some(self.home.as_raw_fd()),
            ".finch",
            AtFlags::AT_SYMLINK_NOFOLLOW,
        );
        match (self.finch_identity, observed_finch) {
            (None, Err(nix::errno::Errno::ENOENT)) => Ok(()),
            (Some(expected), Ok(stat))
                if (stat.st_dev, stat.st_ino) == expected
                    && nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode)
                        .contains(nix::sys::stat::SFlag::S_IFDIR) =>
            {
                Ok(())
            }
            _ => anyhow::bail!("real Finch state identity changed"),
        }
    }
}

fn node_identity_digest(finch: Option<&File>) -> anyhow::Result<String> {
    use nix::fcntl::{openat, readlinkat, AtFlags, OFlag};
    use nix::sys::stat::{fstat, fstatat, Mode, SFlag};
    let Some(directory) = finch else {
        return Ok(hex::encode(Sha256::digest(b"missing")));
    };
    let observed = match fstatat(
        Some(directory.as_raw_fd()),
        "node_id",
        AtFlags::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(nix::errno::Errno::ENOENT) => return Ok(hex::encode(Sha256::digest(b"missing"))),
        Err(error) => return Err(error.into()),
    };
    let mut digest = Sha256::new();
    hash_manifest_stat(&mut digest, Path::new("node_id"), &observed);
    hash_node_identity_times(&mut digest, &observed);
    let kind = SFlag::from_bits_truncate(observed.st_mode);
    if kind.contains(SFlag::S_IFLNK) {
        digest.update(b"link");
        digest.update(readlinkat(Some(directory.as_raw_fd()), "node_id")?.as_encoded_bytes());
    } else if kind.contains(SFlag::S_IFREG) {
        digest.update(b"file");
        let fd = openat(
            Some(directory.as_raw_fd()),
            "node_id",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let file = unsafe { File::from_raw_fd(fd) };
        let opened = fstat(file.as_raw_fd())?;
        anyhow::ensure!(
            SFlag::from_bits_truncate(opened.st_mode).contains(SFlag::S_IFREG)
                && opened.st_dev == observed.st_dev
                && opened.st_ino == observed.st_ino,
            "real node identity changed while opening"
        );
        let mut contents = Vec::new();
        file.take((1 << 20) + 1).read_to_end(&mut contents)?;
        anyhow::ensure!(
            contents.len() <= 1 << 20,
            "real node identity exceeds its size bound"
        );
        digest.update(contents);
    } else {
        digest.update(b"other");
    }
    Ok(hex::encode(digest.finalize()))
}

fn create_proof(
    home: &Path,
    socket_root: &Path,
    ipc_listener: &UnixListener,
    brain_address: &str,
    daemon_address: &str,
    password: &str,
    signing_key: &SigningKey,
) -> anyhow::Result<(File, String)> {
    let root = home.join(".finch/brains");
    let socket = socket_root.join("daemon.sock");
    let home_metadata = fs::metadata(home)?;
    let root_metadata = fs::metadata(&root)?;
    let socket_root_metadata = fs::metadata(socket_root)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let path = home.join(format!(".proof-{token}"));
    let mut contents = String::new();
    use std::fmt::Write as _;
    writeln!(contents, "{token}")?;
    writeln!(contents, "{}", home.display())?;
    writeln!(contents, "{}", root.display())?;
    writeln!(contents, "{}:{}", home_metadata.dev(), home_metadata.ino())?;
    writeln!(contents, "{}:{}", root_metadata.dev(), root_metadata.ino())?;
    writeln!(contents, "{brain_address}")?;
    writeln!(contents, "{daemon_address}")?;
    writeln!(
        contents,
        "{}",
        hex::encode(Sha256::digest(password.as_bytes()))
    )?;
    writeln!(contents, "{}", socket.display())?;
    writeln!(contents, "{}", socket_root.display())?;
    writeln!(
        contents,
        "{}:{}",
        socket_root_metadata.dev(),
        socket_root_metadata.ino()
    )?;
    let ipc_listener_stat = nix::sys::stat::fstat(ipc_listener.as_raw_fd())?;
    writeln!(
        contents,
        "{}",
        unsigned_identity(
            ipc_listener_stat.st_dev as u64,
            ipc_listener_stat.st_ino as u64,
        )
    )?;
    writeln!(contents, "{}", std::process::id())?;
    let supervisor_executable = std::env::current_exe()?.canonicalize()?;
    let supervisor_metadata = fs::metadata(&supervisor_executable)?;
    writeln!(contents, "{}", supervisor_executable.display())?;
    writeln!(
        contents,
        "{}:{}",
        supervisor_metadata.dev(),
        supervisor_metadata.ino()
    )?;
    // The inode pair alone cannot tell a rebuild from a substitution. Cargo
    // replaces a binary by writing a new file and renaming it into place, so a
    // legitimate relink of a workspace target allocates a new inode and looks
    // exactly like someone swapping the program. Recording what the image
    // *contains* separates the two (#259).
    writeln!(
        contents,
        "{}",
        hex::encode(Sha256::digest(fs::read(&supervisor_executable)?))
    )?;
    let signature = signing_key.sign(contents.as_bytes());
    writeln!(contents, "{}", hex::encode(signature.to_bytes()))?;

    let mut writer = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    writer.write_all(contents.as_bytes())?;
    writer.sync_all()?;
    writer.set_permissions(fs::Permissions::from_mode(0o400))?;
    drop(writer);
    let proof = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let flags = unsafe { libc::fcntl(proof.as_raw_fd(), libc::F_GETFL) };
    anyhow::ensure!(
        flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDONLY,
        "proof descriptor is not read-only"
    );
    fs::remove_file(path)?;
    Ok((proof, token))
}

fn configure_supervised_child(
    command: &mut Command,
    proof_fd: RawFd,
    auth_fd: RawFd,
    brain_fd: RawFd,
    daemon_fd: RawFd,
    ipc_fd: RawFd,
) {
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // FD108 is the sealed proof authority. FD9 is the production
            // target restored from it because script interpreters may use a
            // low descriptor while reading a launcher.
            for target in [9, 108] {
                if proof_fd != target && libc::dup2(proof_fd, target) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(target, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            if auth_fd != 109 && libc::dup2(auth_fd, 109) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(109, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Cargo uses low-numbered descriptors for its GNU jobserver while
            // launching a test binary. Keep sealed backups above that range;
            // the test process restores and authenticates the specified
            // FD10/FD11/FD12 production boundary before using any listener.
            for (source, target) in [
                (brain_fd, 10),
                (daemon_fd, 11),
                (ipc_fd, 12),
                (brain_fd, 110),
                (daemon_fd, 111),
                (ipc_fd, 112),
            ] {
                if source != target && libc::dup2(source, target) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(target, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

fn duplicate_above_stdio(fd: RawFd) -> io::Result<File> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 200) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicate) })
}

fn service_proof_authentication(
    socket: &UnixDatagram,
    verifying_key: &[u8; 32],
) -> anyhow::Result<()> {
    let mut request = [0_u8; 64];
    let deadline = Instant::now() + Duration::from_millis(10);
    let mut serviced = 0_u16;
    loop {
        if serviced == 256 || Instant::now() >= deadline {
            return Ok(());
        }
        match socket.recv(&mut request) {
            Ok(count) => {
                anyhow::ensure!(
                    count == b"finch-proof-key-v1".len(),
                    "invalid proof-auth request"
                );
                anyhow::ensure!(
                    &request[..count] == b"finch-proof-key-v1",
                    "invalid proof-auth request"
                );
                socket.send(verifying_key)?;
                serviced += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn set_descriptor_flag(fd: RawFd, command: libc::c_int, value: libc::c_int) -> io::Result<()> {
    if unsafe { libc::fcntl(fd, command, value) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn leader_exited(child: &Child) -> io::Result<bool> {
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut information,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { information.si_pid() } != 0)
}

fn bounded_ps_output() -> anyhow::Result<Vec<u8>> {
    let mut child = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("inspect supervised process group")?;
    let mut stdout = child.stdout.take().context("capture process inventory")?;
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    anyhow::ensure!(flags >= 0, "could not inspect process inventory pipe flags");
    set_descriptor_flag(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)?;
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut status = None;
    loop {
        let mut eof = false;
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    anyhow::ensure!(
                        output.len() + count <= 1024 * 1024,
                        "process inventory exceeded its output bound"
                    );
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if let Some(status) = status.filter(|_| eof) {
            anyhow::ensure!(
                status.success(),
                "could not inspect supervised process group"
            );
            return Ok(output);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("process-group inspection exceeded its one-second deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn process_group_members(group: libc::pid_t) -> anyhow::Result<Vec<libc::pid_t>> {
    if std::env::var_os("FINCH_TEST_FORCE_GROUP_INSPECTION_FAILURE").is_some() {
        anyhow::bail!("forced test process-group inspection failure");
    }
    // PATH belongs to the supervised child. Process membership is a cleanup
    // authority decision, so invoke the platform-owned executable directly.
    let output = bounded_ps_output()?;
    let mut members = Vec::new();
    for line in String::from_utf8(output)?.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid): Option<libc::pid_t> = fields.next().and_then(|value| value.parse().ok())
        else {
            continue;
        };
        let Some(pgid): Option<libc::pid_t> = fields.next().and_then(|value| value.parse().ok())
        else {
            continue;
        };
        if pgid == group && pid != group {
            members.push(pid);
        }
    }
    Ok(members)
}

fn signal_process_group(group: libc::pid_t, signal: libc::c_int) -> anyhow::Result<()> {
    let result = unsafe { libc::kill(-group, signal) };
    if result == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn drain_pipe(read_fd: RawFd) {
    let mut buffer = [0_u8; 32];
    unsafe {
        libc::read(read_fd, buffer.as_mut_ptr().cast(), buffer.len());
    }
}

fn wait_for_event(read_fd: RawFd, timeout_ms: i32) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: read_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
        return Err(io::Error::last_os_error());
    }
    if result > 0 {
        drain_pipe(read_fd);
    }
    Ok(())
}

fn terminate_and_reap(child: &mut Child, group: libc::pid_t) -> anyhow::Result<ExitStatus> {
    // On macOS, signaling a group whose only member is an unreaped zombie
    // leader can return EPERM. It is already quiescent, so retain the leader
    // identity and proceed directly to the one final wait.
    if leader_exited(child)? && process_group_members(group)?.is_empty() {
        return Ok(child.wait()?);
    }
    // Each stage re-signals while it waits, so a member that arrives late --
    // or one that was blocking signals when the first went out -- still gets
    // one. Escalation is what the bound is for, so an expired SIGTERM stage is
    // not an error; it is the reason SIGKILL follows.
    let quiescent = |child: &mut Child| -> anyhow::Result<bool> {
        Ok(leader_exited(child)? && process_group_members(group)?.is_empty())
    };

    signal_process_group(group, libc::SIGTERM)?;
    let term_deadline = Instant::now() + TEARDOWN_SIGTERM_BOUND;
    while Instant::now() < term_deadline {
        if quiescent(child)? {
            break;
        }
        signal_process_group(group, libc::SIGTERM)?;
        std::thread::sleep(POLL_INTERVAL);
    }

    if !quiescent(child)? {
        signal_process_group(group, libc::SIGKILL)?;
        let group_for_context = group;
        let leader = child.id();
        await_bounded(
            "teardown",
            "the owned process group to become quiescent after SIGKILL",
            TEARDOWN_SIGKILL_BOUND,
            || {
                signal_process_group(group, libc::SIGKILL)?;
                Ok(leader_exited(child)? && process_group_members(group)?.is_empty())
            },
            || {
                let survivors = process_group_members(group_for_context)
                    .map(|members| format!("{members:?}"))
                    .unwrap_or_else(|error| format!("<unreadable: {error}>"));
                format!(
                    "leader pid {leader}, process group {group_for_context}, \
                     surviving members {survivors}. SIGTERM had already been \
                     given {TEARDOWN_SIGTERM_BOUND:?}. HOME is preserved rather \
                     than cleaned under a live group"
                )
            },
        )?;
    }
    // Reaping is the final lifecycle operation. The unreaped leader pins the
    // PGID throughout every group signal and membership check above, so the
    // kernel cannot reuse it as an unrelated process group before this wait.
    let status = child.wait()?;
    Ok(status)
}

struct OwnedProcessGroup {
    child: Child,
    group: libc::pid_t,
    reaped: bool,
}

impl OwnedProcessGroup {
    fn new(child: Child) -> Self {
        let group = child.id() as libc::pid_t;
        Self {
            child,
            group,
            reaped: false,
        }
    }

    fn finish(&mut self) -> anyhow::Result<ExitStatus> {
        let status = terminate_and_reap(&mut self.child, self.group)?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = signal_process_group(self.group, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(100));
        let _ = signal_process_group(self.group, libc::SIGKILL);
        let mut quiescent = false;
        let deadline = Instant::now() + TEARDOWN_SIGKILL_BOUND;
        while Instant::now() < deadline {
            if leader_exited(&self.child).unwrap_or(false)
                && process_group_members(self.group).is_ok_and(|members| members.is_empty())
            {
                quiescent = true;
                break;
            }
            let _ = signal_process_group(self.group, libc::SIGKILL);
            std::thread::sleep(POLL_INTERVAL);
        }
        if !quiescent {
            // Drop cannot return an error, and leaving no trace here is how a
            // teardown failure gets misread later as an unexplained dirty
            // HOME. Name the survivors on the way out.
            let survivors = process_group_members(self.group)
                .map(|members| format!("{members:?}"))
                .unwrap_or_else(|error| format!("<unreadable: {error}>"));
            eprintln!(
                "finch-test-supervisor: process group {} did not become quiescent \
                 within {:?} of SIGKILL during drop; surviving members {}; the \
                 leader stays unreaped so the kernel cannot reuse the PGID",
                self.group, TEARDOWN_SIGKILL_BOUND, survivors
            );
        }
        // Never reap the leader while another member might remain: the zombie
        // pins the PGID against reuse until this supervisor exits. Error paths
        // preserve the isolated HOME rather than cleaning state under a live
        // group.
        if quiescent {
            let _ = self.child.wait();
        }
    }
}

fn run() -> anyhow::Result<i32> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        !arguments.is_empty(),
        "finch-test-supervisor requires a command"
    );
    let real_home = canonical_directory(
        "real HOME",
        &PathBuf::from(
            std::env::var_os("FINCH_TEST_REAL_HOME")
                .or_else(|| std::env::var_os("HOME"))
                .ok_or_else(|| anyhow::anyhow!("HOME is unavailable"))?,
        ),
    )?;
    let temp_parent_input = match std::env::var_os("FINCH_TEST_TMP_PARENT") {
        Some(path) => PathBuf::from(path),
        None => std::env::temp_dir()
            .canonicalize()
            .context("could not canonicalize the platform temporary directory")?,
    };
    let temp_parent = canonical_directory("temporary parent", &temp_parent_input)?;
    anyhow::ensure!(
        !matches!(real_home.as_path(), p if p == Path::new("/") || p == Path::new("/tmp") || p == Path::new("/var") || p == Path::new("/private")),
        "real HOME is too broad"
    );
    let real_store = resolve_real_store(&real_home)?;
    anyhow::ensure!(
        !temp_parent.starts_with(&real_store) && !real_store.starts_with(&temp_parent),
        "temporary parent overlaps the real Brain store"
    );
    let isolated = tempfile::Builder::new()
        .prefix("finch-brain-test-home.")
        .tempdir_in(&temp_parent)?;
    fs::set_permissions(isolated.path(), fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(isolated.path().join(".finch/brains"))?;
    fs::set_permissions(
        isolated.path().join(".finch"),
        fs::Permissions::from_mode(0o700),
    )?;
    for relative in [
        ".config",
        ".cache",
        ".cache/huggingface",
        ".cache/huggingface/hub",
        ".cache/huggingface/transformers",
        ".local",
        ".local/share",
        ".local/state",
        "tmp",
    ] {
        let directory = isolated.path().join(relative);
        fs::create_dir_all(&directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let real_node_identity = RealNodeIdentityGuard::pin(&real_home)?;
    let before = manifest_digest(&real_store, Some(isolated.path()))?;
    let node_identity_before = node_identity_digest(real_node_identity.finch.as_ref())?;
    let socket_parent = if Path::new("/private/tmp").is_dir() {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    };
    let socket_root = tempfile::Builder::new()
        .prefix("ft.")
        .tempdir_in(socket_parent)?;
    fs::set_permissions(socket_root.path(), fs::Permissions::from_mode(0o700))?;
    // Bind the Unix listener while the supervisor still owns the verified
    // directory. Supervised children inherit the open listener and therefore
    // never resolve, unlink, create, or bind its pathname themselves.
    let ipc_listener = UnixListener::bind(socket_root.path().join("daemon.sock"))?;
    let brain_listener = TcpListener::bind("127.0.0.1:0")?;
    let daemon_listener = TcpListener::bind("127.0.0.1:0")?;
    let brain_address = brain_listener.local_addr()?.to_string();
    let daemon_address = daemon_listener.local_addr()?.to_string();
    let password = format!("test-{}", uuid::Uuid::new_v4().simple());
    let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
    let verifying_key = signing_key.verifying_key().to_bytes();
    let (proof, token) = create_proof(
        isolated.path(),
        socket_root.path(),
        &ipc_listener,
        &brain_address,
        &daemon_address,
        &password,
        &signing_key,
    )?;
    let (auth_parent, auth_child_socket) = UnixDatagram::pair()?;
    auth_parent.set_nonblocking(true)?;
    let mut pipes = [0; 2];
    if unsafe { libc::pipe(pipes.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    set_descriptor_flag(pipes[0], libc::F_SETFL, libc::O_NONBLOCK)?;
    set_descriptor_flag(pipes[1], libc::F_SETFL, libc::O_NONBLOCK)?;
    set_descriptor_flag(pipes[0], libc::F_SETFD, libc::FD_CLOEXEC)?;
    set_descriptor_flag(pipes[1], libc::F_SETFD, libc::FD_CLOEXEC)?;
    install_signal_handlers(pipes[1])?;
    let mut command = Command::new(&arguments[0]);
    command
        .args(&arguments[1..])
        .env("HOME", isolated.path())
        .env("XDG_CONFIG_HOME", isolated.path().join(".config"))
        .env("XDG_CACHE_HOME", isolated.path().join(".cache"))
        .env("XDG_DATA_HOME", isolated.path().join(".local/share"))
        .env("XDG_STATE_HOME", isolated.path().join(".local/state"))
        .env("HF_HOME", isolated.path().join(".cache/huggingface"))
        .env(
            "HUGGINGFACE_HUB_CACHE",
            isolated.path().join(".cache/huggingface/hub"),
        )
        .env(
            "TRANSFORMERS_CACHE",
            isolated.path().join(".cache/huggingface/transformers"),
        )
        .env("TMPDIR", isolated.path().join("tmp"))
        .env("FINCH_BRAIN_TEST_HOME", isolated.path())
        .env(
            "FINCH_BRAIN_TEST_ROOT",
            isolated.path().join(".finch/brains"),
        )
        .env(
            "FINCH_TEST_IPC_SOCKET",
            socket_root.path().join("daemon.sock"),
        )
        .env("FINCH_TEST_SOCKET_ROOT", socket_root.path())
        .env("FINCH_BRAIN_TEST_ISOLATED", "1")
        .env("FINCH_BRAIN_TEST_TOKEN", &token)
        .env("FINCH_BRAIN_TEST_PROOF_FD", "9")
        .env("FINCH_BRAIN_TEST_PROOF_BACKUP_FD", "108")
        .env("FINCH_BRAIN_TEST_AUTH_FD", "109")
        .env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1")
        .env("FINCH_TEST_BRAIN_ADDR", &brain_address)
        .env("FINCH_TEST_DAEMON_ADDR", &daemon_address)
        .env("FINCH_TEST_BRAIN_PASSWORD", &password)
        .env("FINCH_TEST_BRAIN_LISTENER_FD", "10")
        .env("FINCH_TEST_DAEMON_LISTENER_FD", "11")
        .env("FINCH_TEST_IPC_LISTENER_FD", "12")
        .env("FINCH_TEST_BRAIN_LISTENER_BACKUP_FD", "110")
        .env("FINCH_TEST_DAEMON_LISTENER_BACKUP_FD", "111")
        .env("FINCH_TEST_IPC_LISTENER_BACKUP_FD", "112")
        .env("FINCH_TEST_SUPERVISOR_PID", std::process::id().to_string())
        .env("FINCH_TEST_SUPERVISOR_BIN", std::env::current_exe()?);
    if std::env::var_os("CARGO_HOME").is_none() && real_home.join(".cargo").is_dir() {
        command.env("CARGO_HOME", real_home.join(".cargo"));
    }
    if std::env::var_os("RUSTUP_HOME").is_none() && real_home.join(".rustup").is_dir() {
        command.env("RUSTUP_HOME", real_home.join(".rustup"));
    }
    for name in [
        "FINCH_TEST_REAL_HOME",
        "FINCH_TEST_TMP_PARENT",
        "FINCH_TEST_PROCESS_REGISTRY",
        "FINCH_TEST_BOUND_ADDR_FILE",
        "FINCH_TEST_FORCE_MANIFEST_AFTER_ERROR",
        "FINCH_TEST_NODE_AFTER_MARKER",
        "FINCH_TEST_REPORT_NODE_AFTER",
        "BRAIN_ADDR",
        "DAEMON_ADDR",
        "BRAIN_PASSWORD",
        "FINCH_WIRE_CORPUS_PATH",
    ] {
        command.env_remove(name);
    }
    let proof_child = duplicate_above_stdio(proof.as_raw_fd())?;
    let auth_child = duplicate_above_stdio(auth_child_socket.as_raw_fd())?;
    let brain_child = duplicate_above_stdio(brain_listener.as_raw_fd())?;
    let daemon_child = duplicate_above_stdio(daemon_listener.as_raw_fd())?;
    let ipc_child = duplicate_above_stdio(ipc_listener.as_raw_fd())?;
    configure_supervised_child(
        &mut command,
        proof_child.as_raw_fd(),
        auth_child.as_raw_fd(),
        brain_child.as_raw_fd(),
        daemon_child.as_raw_fd(),
        ipc_child.as_raw_fd(),
    );
    let mut group = OwnedProcessGroup::new(command.spawn()?);
    let observation = (|| -> anyhow::Result<()> {
        while PENDING_SIGNAL.load(Ordering::Relaxed) == 0 && !leader_exited(&group.child)? {
            service_proof_authentication(&auth_parent, &verifying_key)?;
            wait_for_event(pipes[0], 25)?;
        }
        service_proof_authentication(&auth_parent, &verifying_key)?;
        Ok(())
    })();
    let status = match group.finish() {
        Ok(status) => status,
        Err(error) => {
            let preserved = isolated.keep();
            let preserved_socket_root = socket_root.keep();
            return Err(error.context(format!(
                "isolated HOME preserved at {} and socket root at {} because the process group was not quiescent",
                preserved.display(), preserved_socket_root.display()
            )));
        }
    };
    // Once the group is proven quiescent, always take the parent-held after
    // snapshot before propagating an observation error or removing HOME.
    let after_result = if std::env::var_os("FINCH_TEST_FORCE_MANIFEST_AFTER_ERROR").is_some() {
        // Executable-level regression hook: prove that an independent node
        // snapshot is still collected when the Brain snapshot itself fails.
        Err(anyhow::anyhow!("forced manifest snapshot failure"))
    } else {
        manifest_digest(&real_store, Some(isolated.path()))
    };
    let node_identity_after_result = (|| {
        real_node_identity.verify_pathnames(&real_home)?;
        node_identity_digest(real_node_identity.finch.as_ref())
    })();
    if std::env::var("FINCH_TEST_REPORT_NODE_AFTER").as_deref() == Ok("1") {
        eprintln!("FINCH_TEST_NODE_AFTER_OBSERVED");
    }
    let after = after_result?;
    let node_identity_after = node_identity_after_result?;
    anyhow::ensure!(
        before == after,
        "real Brain store manifest changed (sha256={before} -> sha256={after})"
    );
    anyhow::ensure!(
        node_identity_before == node_identity_after,
        "real node identity changed (sha256={node_identity_before} -> sha256={node_identity_after})"
    );
    observation?;
    drop(proof);
    drop(isolated);
    drop(socket_root);
    // A signal arriving while TERM/KILL escalation was in progress must still
    // control the wrapper's exit status.
    let signal = PENDING_SIGNAL.load(Ordering::Relaxed);
    if signal != 0 {
        Ok(128 + signal)
    } else {
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }
}

fn run_child_panic_probe() -> ! {
    let descendant = Command::new("/bin/sh")
        .args(["-c", "trap '' TERM HUP INT; while :; do /bin/sleep 1; done"])
        .spawn()
        .expect("panic probe descendant must spawn");
    if let Some(path) = std::env::var_os("FINCH_TEST_PANIC_DESCENDANT_PID_FILE") {
        fs::write(path, descendant.id().to_string()).expect("panic probe PID must be recorded");
    }
    if let Some(path) = std::env::var_os("FINCH_TEST_PANIC_HOME_FILE") {
        fs::write(
            path,
            std::env::var("HOME").expect("panic probe requires HOME"),
        )
        .expect("panic probe HOME must be recorded");
    }
    panic!("intentional supervised child panic probe");
}

fn run_child_socket_manifest_probe() -> anyhow::Result<()> {
    let store = PathBuf::from(
        std::env::var_os("FINCH_REAL_STORE")
            .context("socket manifest probe requires FINCH_REAL_STORE")?,
    );
    let socket = store.join("secret-socket");
    let _listener = std::os::unix::net::UnixListener::bind(socket)?;
    Ok(())
}

fn run_child_stubborn_probe() -> anyhow::Result<()> {
    let ready = std::env::var_os("FINCH_STUBBORN_READY_FILE")
        .context("stubborn probe requires ready path")?;
    let terminated = std::env::var_os("FINCH_STUBBORN_TERM_FILE")
        .context("stubborn probe requires termination path")?;
    let mut pipes = [0; 2];
    if unsafe { libc::pipe(pipes.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    set_descriptor_flag(pipes[0], libc::F_SETFL, libc::O_NONBLOCK)?;
    set_descriptor_flag(pipes[1], libc::F_SETFL, libc::O_NONBLOCK)?;
    install_signal_handlers(pipes[1])?;
    fs::write(ready, b"ready\n")?;
    let pause_after_first = std::env::var_os("FINCH_STUBBORN_TERM_PAUSE_AFTER_FIRST_FILE");
    let mut publications = 0_u64;
    loop {
        wait_for_event(pipes[0], 1000)?;
        if PENDING_SIGNAL.swap(0, Ordering::Relaxed) != 0 {
            // Repeated SIGTERM is intentional during bounded group teardown.
            // `fs::write` opened with truncate semantics, so SIGKILL between a
            // later truncate and write could erase evidence already observed
            // by the parent (#283). Opening append-only makes every successful
            // publication monotonic; an interrupted later write can add
            // nothing (or a prefix), but cannot remove the first record.
            let mut marker = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&terminated)?;
            if let (true, Some(ready_path)) = (publications > 0, pause_after_first.as_ref()) {
                // Production-boundary fault injection at the historical
                // destructive window: the old create/truncate happened
                // before this pause. The real supervisor must reach its
                // SIGKILL bound while this fixture remains parked.
                fs::write(ready_path, b"later publication paused\n")?;
                // Derived from the teardown bound rather than chosen to sit
                // above it: this fixture only proves anything while it
                // outlives the supervisor's escalation.
                let deadline = Instant::now() + STUBBORN_FIXTURE_PAUSE;
                while Instant::now() < deadline {
                    std::thread::sleep(POLL_INTERVAL);
                }
                anyhow::bail!(
                    "stubborn marker pause of {STUBBORN_FIXTURE_PAUSE:?} outlived the \
                     supervisor teardown bound of {:?}; the supervisor did not reach \
                     its SIGKILL escalation",
                    Duration::from_secs(TEARDOWN_BOUND_SECS)
                );
            }
            marker.write_all(b"term\n")?;
            publications += 1;
        }
    }
}

fn read_http_fixture_request(
    stream: &std::net::TcpStream,
) -> anyhow::Result<(String, String, String)> {
    use std::io::BufRead as _;

    let mut reader = io::BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut fields = request_line.split_whitespace();
    let method = fields
        .next()
        .context("HTTP fixture request has no method")?;
    let path = fields.next().context("HTTP fixture request has no path")?;
    let mut content_length = 0_usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse()?;
        }
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body)?;
    Ok((method.to_owned(), path.to_owned(), String::from_utf8(body)?))
}

fn run_child_http_fixture() -> anyhow::Result<()> {
    let proof = finch::brain::isolated_test_proof()
        .context("HTTP fixture requires authenticated supervisor authority")?;
    let expected = std::env::var("FINCH_TEST_DAEMON_ADDR")
        .context("HTTP fixture is missing the sealed daemon address")?;
    anyhow::ensure!(
        expected == proof.daemon_address(),
        "HTTP fixture daemon address escaped supervisor authority"
    );
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        arguments == ["daemon", "--bind", expected.as_str()],
        "HTTP fixture received an unexpected daemon command"
    );
    let duplicate = unsafe { libc::dup(11) };
    anyhow::ensure!(duplicate >= 0, "HTTP fixture listener FD11 is unavailable");
    let listener = unsafe { TcpListener::from_raw_fd(duplicate) };
    let actual = listener.local_addr()?.to_string();
    anyhow::ensure!(
        actual == expected,
        "HTTP fixture listener authority mismatch"
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var_os("FINCH_MOCK_BIND_LOG").context("missing fixture bind log")?)?
        .write_all(format!("{expected}|{actual}\n").as_bytes())?;
    fs::write(
        std::env::var_os("FINCH_TEST_BOUND_ADDR_FILE").context("missing fixture address file")?,
        actual,
    )?;

    for connection in listener.incoming() {
        let mut stream = connection?;
        let (method, path, body) = read_http_fixture_request(&stream)?;
        let compact_body = body
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let (status, kind, response) = match (method.as_str(), path.as_str()) {
            ("GET", "/health") => (200, "application/json", r#"{"status":"ok"}"#),
            ("GET", "/metrics") => (200, "text/plain", "finch_test 1\n"),
            ("POST", _) if compact_body.contains(r#""role":"tool""#) => (
                200,
                "application/json",
                r#"{"choices":[{"message":{"content":"tool result accepted"}}]}"#,
            ),
            ("POST", _) => (
                200,
                "application/json",
                r#"{"choices":[{"message":{"tool_calls":[{"id":"call_test","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
            ),
            _ => (404, "application/json", "{}"),
        };
        write!(
            stream,
            "HTTP/1.1 {status} OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )?;
    }
    Ok(())
}

fn exit_fixture(result: anyhow::Result<()>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("Brain test fixture: {error:#}");
            std::process::exit(70);
        }
    }
}

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--child-panic-probe"))
        && std::env::args_os().nth(2).is_none()
    {
        run_child_panic_probe();
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--child-socket-manifest-probe"))
        && std::env::args_os().nth(2).is_none()
    {
        exit_fixture(run_child_socket_manifest_probe());
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--child-stubborn-probe"))
        && std::env::args_os().nth(2).is_none()
    {
        exit_fixture(run_child_stubborn_probe());
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--verify-inherited-proof"))
        && std::env::args_os().nth(2).is_none()
    {
        let status = match finch::brain::authenticated_isolated_test_proof_text() {
            Ok(contents) => io::stdout().write_all(&contents).map(|_| 0).unwrap_or(1),
            Err(error) => {
                // The synthetic harness enables this only while diagnosing its
                // own inherited-descriptor contract. Display the outer
                // predicate, never proof contents, credentials, or paths.
                if std::env::var("FINCH_TEST_PROOF_DIAGNOSTICS").as_deref() == Ok("1") {
                    eprintln!("Brain test proof verification failed: {error}");
                }
                1
            }
        };
        std::process::exit(status);
    }
    if std::env::var("FINCH_TEST_HTTP_FIXTURE").as_deref() == Ok("1")
        && std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Ok("1")
    {
        exit_fixture(run_child_http_fixture());
    }
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("Brain test supervisor: {error:#}");
            std::process::exit(70);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn signed_device_bits_serialize_as_parseable_u64_identity() {
        let signed_device = -1_i32;
        let encoded = super::unsigned_identity(signed_device as u64, 42);
        let (device, inode) = encoded.split_once(':').unwrap();
        assert_eq!(device.parse::<u64>().unwrap(), signed_device as u64);
        assert_eq!(inode.parse::<u64>().unwrap(), 42);
    }

    /// The fixture that races teardown must outlive it (#328).
    ///
    /// `run_child_stubborn_probe` parks and asserts the supervisor kills it
    /// first, so it only proves anything while its pause exceeds the
    /// supervisor's escalation. Both were bare literals -- 5s against 2s+2s --
    /// and raising the teardown bound for load tolerance would have inverted
    /// the ordering silently, turning a real teardown regression into a
    /// passing test. The constants are derived now; this fails if that stops
    /// being true.
    #[test]
    fn the_stubborn_fixture_outlives_the_teardown_bound() {
        assert!(
            super::STUBBORN_FIXTURE_PAUSE_SECS > super::TEARDOWN_BOUND_SECS,
            "the stubborn fixture parks for {}s but the supervisor may spend \
             {}s tearing down ({}s SIGTERM + {}s SIGKILL); the fixture would \
             give up before the supervisor is obliged to have killed it, and \
             would pass while proving nothing",
            super::STUBBORN_FIXTURE_PAUSE_SECS,
            super::TEARDOWN_BOUND_SECS,
            super::TEARDOWN_SIGTERM_SECS,
            super::TEARDOWN_SIGKILL_SECS
        );
    }

    /// A wait that is satisfied late still succeeds.
    ///
    /// This is the #328 case: the supervised process reached the expected
    /// state, just later than a fixed window allowed.
    #[test]
    fn a_bounded_wait_succeeds_when_readiness_arrives_late() {
        let mut polls = 0;
        let outcome = super::await_bounded(
            "test",
            "a late condition",
            std::time::Duration::from_secs(5),
            || {
                polls += 1;
                Ok(polls > 6)
            },
            || "unused".into(),
        );
        assert!(
            outcome.is_ok(),
            "a condition that becomes true after {polls} polls must satisfy a \
             bound it fits inside; got {outcome:?}"
        );
    }

    /// On expiry the error must say what was awaited, not just that something
    /// timed out. The failures this replaces carried a line number and nothing
    /// else, which cannot separate a loaded host from a lifecycle regression.
    #[test]
    fn an_expired_wait_reports_the_phase_condition_and_context() {
        let error = super::await_bounded(
            "teardown",
            "the owned process group to become quiescent",
            std::time::Duration::from_millis(30),
            || Ok(false),
            || "leader pid 4242, process group 4242, surviving members [4243]".into(),
        )
        .expect_err("a condition that never holds must expire");
        let rendered = format!("{error}");
        for expected in [
            "teardown",
            "the owned process group to become quiescent",
            "leader pid 4242",
            "surviving members [4243]",
            "30ms",
        ] {
            assert!(
                rendered.contains(expected),
                "an expired wait must report `{expected}` so the reader can \
                 tell a slow host from a regression; got: {rendered}"
            );
        }
    }

    /// The context closure runs only on the failure path, so it may be as
    /// expensive as it needs to be -- reading `ps`, stat-ing paths. A wait
    /// that succeeds must not pay for diagnostics nobody reads.
    #[test]
    fn context_is_not_built_for_a_wait_that_succeeds() {
        let mut built = false;
        super::await_bounded(
            "test",
            "an immediate condition",
            std::time::Duration::from_secs(1),
            || Ok(true),
            || {
                built = true;
                String::new()
            },
        )
        .expect("an immediately true condition succeeds");
        assert!(
            !built,
            "the context closure ran on the success path; it is allowed to be \
             expensive precisely because it should not"
        );
    }

    /// A condition that cannot be evaluated is not a timeout, and must not be
    /// reported as one -- an unreadable process table means the harness has
    /// lost the ability to observe ownership, which is worse news than a slow
    /// child and must not wait out the bound before saying so.
    #[test]
    fn a_failing_condition_surfaces_its_own_error_immediately() {
        let started = std::time::Instant::now();
        let error = super::await_bounded(
            "teardown",
            "quiescence",
            std::time::Duration::from_secs(30),
            || anyhow::bail!("process table unreadable"),
            || "unused".into(),
        )
        .expect_err("a condition that errors must propagate");
        assert!(
            format!("{error}").contains("process table unreadable"),
            "the condition's own error must survive; got: {error}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "an unevaluable condition must fail immediately, not wait out the \
             bound; took {:?}",
            started.elapsed()
        );
    }
}
