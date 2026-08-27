//! Test-only Finch process and filesystem isolation supervisor.

#![cfg(unix)]

use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use nix::libc;

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

fn hash_node(path: &Path, relative: &Path, digest: &mut Sha256) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    digest.update(relative.as_os_str().as_encoded_bytes());
    digest.update(metadata.mode().to_ne_bytes());
    digest.update(metadata.uid().to_ne_bytes());
    digest.update(metadata.gid().to_ne_bytes());
    digest.update(metadata.nlink().to_ne_bytes());
    digest.update(metadata.dev().to_ne_bytes());
    digest.update(metadata.ino().to_ne_bytes());
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        digest.update(b"link");
        digest.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
    } else if file_type.is_file() {
        digest.update(b"file");
        let mut file = File::open(path)?;
        let mut buffer = [0_u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    } else if file_type.is_dir() {
        digest.update(b"dir");
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            hash_node(&entry.path(), &relative.join(entry.file_name()), digest)?;
        }
    } else if file_type.is_socket() {
        digest.update(b"socket");
    } else if file_type.is_fifo() {
        digest.update(b"fifo");
    } else if file_type.is_block_device() {
        digest.update(b"block");
    } else if file_type.is_char_device() {
        digest.update(b"char");
    } else {
        digest.update(b"other");
    }
    Ok(())
}

fn manifest_digest(store: &Path) -> anyhow::Result<String> {
    if !store.exists() {
        return Ok(hex::encode(Sha256::digest(b"missing")));
    }
    let mut digest = Sha256::new();
    hash_node(store, Path::new("."), &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

fn create_proof(
    home: &Path,
    socket_root: &Path,
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
            // FD10/FD11 production boundary before using either listener.
            for (source, target) in [
                (brain_fd, 10),
                (daemon_fd, 11),
                (brain_fd, 110),
                (daemon_fd, 111),
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
    loop {
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

fn process_group_members(group: libc::pid_t) -> anyhow::Result<Vec<libc::pid_t>> {
    if std::env::var_os("FINCH_TEST_FORCE_GROUP_INSPECTION_FAILURE").is_some() {
        anyhow::bail!("forced test process-group inspection failure");
    }
    // PATH belongs to the supervised child. Process membership is a cleanup
    // authority decision, so invoke the platform-owned executable directly.
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid="])
        .output()
        .context("inspect supervised process group")?;
    anyhow::ensure!(
        output.status.success(),
        "could not inspect supervised process group"
    );
    let mut members = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
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
    signal_process_group(group, libc::SIGTERM)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if leader_exited(child)? && process_group_members(group)?.is_empty() {
            break;
        }
        signal_process_group(group, libc::SIGTERM)?;
        std::thread::sleep(Duration::from_millis(10));
    }
    if !leader_exited(child)? || !process_group_members(group)?.is_empty() {
        signal_process_group(group, libc::SIGKILL)?;
        let quiescence_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < quiescence_deadline {
            if leader_exited(child)? && process_group_members(group)?.is_empty() {
                break;
            }
            signal_process_group(group, libc::SIGKILL)?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    anyhow::ensure!(
        leader_exited(child)? && process_group_members(group)?.is_empty(),
        "owned test process group did not become quiescent"
    );
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
        for _ in 0..200 {
            if leader_exited(&self.child).unwrap_or(false)
                && process_group_members(self.group).is_ok_and(|members| members.is_empty())
            {
                quiescent = true;
                break;
            }
            let _ = signal_process_group(self.group, libc::SIGKILL);
            std::thread::sleep(Duration::from_millis(10));
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
    let before = manifest_digest(&real_store)?;
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
    let socket_parent = if Path::new("/private/tmp").is_dir() {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    };
    let socket_root = tempfile::Builder::new()
        .prefix("ft.")
        .tempdir_in(socket_parent)?;
    fs::set_permissions(socket_root.path(), fs::Permissions::from_mode(0o700))?;
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
        .env("FINCH_TEST_BRAIN_LISTENER_BACKUP_FD", "110")
        .env("FINCH_TEST_DAEMON_LISTENER_BACKUP_FD", "111")
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
    configure_supervised_child(
        &mut command,
        proof_child.as_raw_fd(),
        auth_child.as_raw_fd(),
        brain_child.as_raw_fd(),
        daemon_child.as_raw_fd(),
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
    let after = manifest_digest(&real_store)?;
    anyhow::ensure!(
        before == after,
        "real Brain store manifest changed (sha256={before} -> sha256={after})"
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
    loop {
        wait_for_event(pipes[0], 1000)?;
        if PENDING_SIGNAL.swap(0, Ordering::Relaxed) != 0 {
            fs::write(&terminated, b"term\n")?;
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
    let expected = std::env::var("FINCH_TEST_DAEMON_ADDR")?;
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
    if std::env::var("FINCH_TEST_HTTP_FIXTURE").as_deref() == Ok("1") {
        exit_fixture(run_child_http_fixture());
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
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("Brain test supervisor: {error:#}");
            std::process::exit(70);
        }
    }
}
