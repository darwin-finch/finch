//! Canonical durable Brain state, credentials, and client transports.
//!
//! Speculative/background activity is represented by `BrainRun` records in
//! the named Brain service. There is deliberately no second client-local
//! "Brain session" or hidden context-injection path here.

pub mod credential;
pub mod names;
pub mod remote;
pub mod store;
pub mod tasks;

#[doc(hidden)]
pub struct IsolatedTestProof {
    pub home: std::path::PathBuf,
    pub root: std::path::PathBuf,
    pub(crate) home_identity: (u64, u64),
    pub(crate) root_identity: (u64, u64),
    pub(crate) brain_addr: String,
    pub(crate) daemon_addr: String,
    pub(crate) ipc_socket: std::path::PathBuf,
    pub(crate) socket_root: std::path::PathBuf,
    pub(crate) socket_root_identity: (u64, u64),
    pub(crate) supervisor_pid: u32,
    pub(crate) password_digest: String,
}

impl IsolatedTestProof {
    #[doc(hidden)]
    pub fn brain_address(&self) -> &str {
        &self.brain_addr
    }

    #[doc(hidden)]
    pub fn daemon_address(&self) -> &str {
        &self.daemon_addr
    }

    #[doc(hidden)]
    pub fn brain_password(&self) -> anyhow::Result<String> {
        let password = std::env::var("FINCH_TEST_BRAIN_PASSWORD")?;
        use sha2::Digest as _;
        anyhow::ensure!(
            hex::encode(sha2::Sha256::digest(password.as_bytes())) == self.password_digest,
            "live credential no longer matches the sealed supervisor authority"
        );
        Ok(password)
    }

    #[cfg(unix)]
    #[doc(hidden)]
    pub fn duplicate_brain_listener(&self) -> anyhow::Result<std::net::TcpListener> {
        duplicate_validated_listener(10, &self.brain_addr)
    }

    #[cfg(unix)]
    #[doc(hidden)]
    pub fn duplicate_daemon_listener(&self) -> anyhow::Result<std::net::TcpListener> {
        duplicate_validated_listener(11, &self.daemon_addr)
    }
}

#[cfg(unix)]
fn duplicate_validated_listener(fd: i32, expected: &str) -> anyhow::Result<std::net::TcpListener> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let socket_option = |name: i32| -> anyhow::Result<i32> {
        let mut value = 0_i32;
        let mut length = std::mem::size_of::<i32>() as nix::libc::socklen_t;
        let result = unsafe {
            nix::libc::getsockopt(
                fd,
                nix::libc::SOL_SOCKET,
                name,
                (&mut value as *mut i32).cast(),
                &mut length,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            anyhow::bail!(
                "supervisor listener descriptor FD {fd} getsockopt({name}) failed \
                 with result {result}: {error}"
            );
        }
        anyhow::ensure!(
            length as usize == std::mem::size_of::<i32>(),
            "supervisor listener descriptor FD {fd} getsockopt({name}) returned invalid length \
             {length}"
        );
        Ok(value)
    };
    anyhow::ensure!(
        socket_option(nix::libc::SO_TYPE)? == nix::libc::SOCK_STREAM,
        "supervisor descriptor is not a TCP stream socket"
    );
    #[cfg(not(target_os = "macos"))]
    anyhow::ensure!(
        socket_option(nix::libc::SO_ACCEPTCONN)? != 0,
        "supervisor descriptor is not a listening TCP stream"
    );
    let duplicate = unsafe { nix::libc::dup(fd) };
    anyhow::ensure!(
        duplicate >= 0,
        "supervisor listener descriptor is unavailable"
    );
    let listener = unsafe { std::net::TcpListener::from_raw_fd(duplicate) };
    let address = listener.local_addr()?;
    anyhow::ensure!(
        address.to_string() == expected && address.ip().is_loopback() && address.port() != 0,
        "supervisor listener address does not match sealed authority"
    );
    #[cfg(target_os = "macos")]
    validate_macos_listener_challenge(listener.as_raw_fd(), address)?;
    Ok(listener)
}

#[cfg(target_os = "macos")]
fn validate_macos_listener_challenge(
    fd: i32,
    expected: std::net::SocketAddr,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::{Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::time::{Duration, Instant};

    static CHALLENGE_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _process_lock = CHALLENGE_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("listener challenge lock is poisoned"))?;

    anyhow::ensure!(
        unsafe { nix::libc::flock(9, nix::libc::LOCK_EX) } == 0,
        "could not lock the sealed proof for listener challenge: {}",
        std::io::Error::last_os_error()
    );
    struct ProofLock;
    impl Drop for ProofLock {
        fn drop(&mut self) {
            unsafe {
                nix::libc::flock(9, nix::libc::LOCK_UN);
            }
        }
    }
    let _proof_lock = ProofLock;

    let original_flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    anyhow::ensure!(
        original_flags >= 0,
        "could not inspect listener flags: {}",
        std::io::Error::last_os_error()
    );
    anyhow::ensure!(
        unsafe {
            nix::libc::fcntl(
                fd,
                nix::libc::F_SETFL,
                original_flags | nix::libc::O_NONBLOCK,
            )
        } == 0,
        "could not make listener nonblocking for challenge: {}",
        std::io::Error::last_os_error()
    );
    struct FileStatusGuard {
        fd: i32,
        original: i32,
        restored: bool,
    }
    impl FileStatusGuard {
        fn restore(mut self) -> anyhow::Result<()> {
            let result = unsafe { nix::libc::fcntl(self.fd, nix::libc::F_SETFL, self.original) };
            anyhow::ensure!(
                result == 0,
                "could not restore listener flags after challenge: {}",
                std::io::Error::last_os_error()
            );
            self.restored = true;
            Ok(())
        }
    }
    impl Drop for FileStatusGuard {
        fn drop(&mut self) {
            if !self.restored {
                unsafe {
                    nix::libc::fcntl(self.fd, nix::libc::F_SETFL, self.original);
                }
            }
        }
    }
    let flags = FileStatusGuard {
        fd,
        original: original_flags,
        restored: false,
    };

    let validation = (|| -> anyhow::Result<()> {
        let duplicate = unsafe { nix::libc::dup(fd) };
        anyhow::ensure!(
            duplicate >= 0,
            "could not duplicate listener for challenge: {}",
            std::io::Error::last_os_error()
        );
        let listener = unsafe { std::net::TcpListener::from_raw_fd(duplicate) };
        let timeout = Duration::from_millis(500);
        let mut client = std::net::TcpStream::connect_timeout(&expected, timeout)
            .context("sealed TCP socket did not accept a loopback challenge")?;
        client.set_write_timeout(Some(timeout))?;
        let nonce = *uuid::Uuid::new_v4().as_bytes();
        client.write_all(&nonce)?;

        let deadline = Instant::now() + timeout;
        let (mut accepted, accepted_peer) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "sealed TCP listener challenge timed out"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        };
        let accepted_flags = unsafe { nix::libc::fcntl(accepted.as_raw_fd(), nix::libc::F_GETFL) };
        anyhow::ensure!(
            accepted_flags >= 0,
            "could not inspect accepted challenge flags: {}",
            std::io::Error::last_os_error()
        );
        anyhow::ensure!(
            unsafe {
                nix::libc::fcntl(
                    accepted.as_raw_fd(),
                    nix::libc::F_SETFL,
                    accepted_flags & !nix::libc::O_NONBLOCK,
                )
            } == 0,
            "could not make accepted challenge stream blocking: {}",
            std::io::Error::last_os_error()
        );
        accepted.set_read_timeout(Some(timeout))?;
        let mut received = [0_u8; 16];
        accepted.read_exact(&mut received)?;
        anyhow::ensure!(
            received == nonce,
            "sealed TCP listener challenge nonce mismatch"
        );
        anyhow::ensure!(
            client.peer_addr()? == expected
                && accepted.local_addr()? == expected
                && client.local_addr()? == accepted_peer
                && accepted.peer_addr()? == client.local_addr()?,
            "sealed TCP listener challenge address pairing mismatch"
        );
        Ok(())
    })();
    let restored = flags.restore();
    restored?;
    validation
}

#[cfg(unix)]
fn restore_supervisor_listener(
    backup_fd: i32,
    target_fd: i32,
    expected: &str,
) -> anyhow::Result<()> {
    // Cargo may occupy FD10/FD11 with its jobserver before it execs libtest.
    // The supervisor therefore retains authenticated, non-CLOEXEC copies at
    // fixed high descriptors. The low descriptor is never authority: validate
    // the sealed backup first, overwrite FD10/FD11 unconditionally, and then
    // authenticate the production-facing descriptor a second time.
    drop(duplicate_validated_listener(backup_fd, expected)?);
    anyhow::ensure!(
        unsafe { nix::libc::dup2(backup_fd, target_fd) } == target_fd,
        "could not restore supervisor listener descriptor"
    );
    anyhow::ensure!(
        unsafe { nix::libc::fcntl(target_fd, nix::libc::F_SETFD, 0) } == 0,
        "could not make restored supervisor listener inheritable"
    );
    drop(duplicate_validated_listener(target_fd, expected)?);
    Ok(())
}

fn process_descends_from(ancestor: u32) -> anyhow::Result<bool> {
    let mut pid = std::process::id();
    let parent = |pid: u32| -> anyhow::Result<u32> {
        let output = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "could not verify supervisor ancestry"
        );
        Ok(String::from_utf8(output.stdout)?.trim().parse()?)
    };
    // The proof issuer must be an actual ancestor. Accepting the current
    // process would let a test manufacture a self-signed environment and FD.
    pid = parent(pid)?;
    while pid > 1 {
        if pid == ancestor {
            return Ok(true);
        }
        pid = parent(pid)?;
    }
    Ok(false)
}

fn expected_supervisor_executable() -> anyhow::Result<std::path::PathBuf> {
    let test_executable = std::env::current_exe()?.canonicalize()?;
    let mut directory = test_executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test executable has no parent"))?;
    if directory.file_name().is_some_and(|name| name == "deps") {
        directory = directory
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test dependency directory has no parent"))?;
    }
    let name = if cfg!(windows) {
        "finch-test-supervisor.exe"
    } else {
        "finch-test-supervisor"
    };
    Ok(directory.join(name).canonicalize()?)
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> anyhow::Result<std::path::PathBuf> {
    Ok(std::fs::read_link(format!("/proc/{pid}/exe"))?)
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> anyhow::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut buffer = vec![0_u8; nix::libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        nix::libc::proc_pidpath(
            pid as nix::libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    anyhow::ensure!(length > 0, "could not resolve supervisor executable");
    buffer.truncate(length as usize);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(
        buffer,
    )))
}

#[doc(hidden)]
pub fn isolated_test_proof() -> anyhow::Result<IsolatedTestProof> {
    use anyhow::Context as _;

    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Ok("1"),
        "live Brain tests require scripts/test_brains.sh"
    );
    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_PROOF_FD").as_deref() == Ok("9"),
        "live Brain tests require the wrapper proof descriptor"
    );
    #[cfg(unix)]
    let mut proof = {
        use std::os::fd::FromRawFd as _;
        let inherited_flags = unsafe { nix::libc::fcntl(9, nix::libc::F_GETFL) };
        anyhow::ensure!(
            inherited_flags >= 0 && inherited_flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY,
            "inherited wrapper proof descriptor is writable or unavailable"
        );
        let duplicate = unsafe { nix::libc::dup(9) };
        anyhow::ensure!(duplicate >= 0, "wrapper proof descriptor is unavailable");
        unsafe { std::fs::File::from_raw_fd(duplicate) }
    };
    #[cfg(not(unix))]
    let mut proof: std::fs::File =
        { anyhow::bail!("Brain test supervisor authority is supported only on Unix") };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = proof.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.nlink() == 0
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.mode() & 0o777 == 0o400,
            "wrapper proof descriptor does not identify the parent-owned sealed file"
        );
    }
    use std::io::{Read as _, Seek as _};
    proof.seek(std::io::SeekFrom::Start(0))?;
    let mut contents = String::new();
    proof.read_to_string(&mut contents)?;
    let mut lines = contents.lines();
    let token = lines.next().context("wrapper proof is missing its nonce")?;
    let home = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its HOME identity")?,
    );
    let root = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its Brain-root identity")?,
    );
    let home_identity = lines
        .next()
        .context("wrapper proof is missing its HOME inode identity")?;
    let root_identity = lines
        .next()
        .context("wrapper proof is missing its Brain-root inode identity")?;
    let brain_addr = lines
        .next()
        .context("wrapper proof is missing its Brain-listener authority")?
        .to_owned();
    let daemon_addr = lines
        .next()
        .context("wrapper proof is missing its daemon-listener authority")?
        .to_owned();
    let password_digest = lines
        .next()
        .context("wrapper proof is missing its password authority")?;
    let ipc_socket = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its IPC-socket authority")?,
    );
    let socket_root = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its socket-root authority")?,
    );
    let socket_root_identity = lines
        .next()
        .context("wrapper proof is missing its socket-root identity")?;
    let supervisor_pid: u32 = lines
        .next()
        .context("wrapper proof is missing its supervisor identity")?
        .parse()?;
    let supervisor_executable = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its supervisor executable")?,
    );
    let supervisor_identity = lines
        .next()
        .context("wrapper proof is missing its supervisor executable identity")?;
    anyhow::ensure!(lines.next().is_none(), "wrapper proof has trailing fields");
    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_TOKEN").as_deref() == Ok(token),
        "wrapper proof nonce does not match the inherited contract"
    );
    anyhow::ensure!(
        std::env::var("FINCH_TEST_SUPERVISOR_PID")
            .ok()
            .and_then(|value| value.parse().ok())
            == Some(supervisor_pid)
            && process_descends_from(supervisor_pid)?
            && process_executable(supervisor_pid)?.canonicalize()? == supervisor_executable
            && supervisor_executable == expected_supervisor_executable()?,
        "proof issuer is not an ancestor test supervisor"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(&supervisor_executable)?;
        anyhow::ensure!(
            supervisor_identity == format!("{}:{}", metadata.dev(), metadata.ino()),
            "supervisor executable identity changed"
        );
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        let flags = unsafe { nix::libc::fcntl(proof.as_raw_fd(), nix::libc::F_GETFL) };
        anyhow::ensure!(
            flags >= 0 && flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY,
            "wrapper proof descriptor is writable"
        );
        anyhow::ensure!(
            std::env::var("FINCH_TEST_BRAIN_LISTENER_FD").as_deref() == Ok("10")
                && std::env::var("FINCH_TEST_DAEMON_LISTENER_FD").as_deref() == Ok("11")
                && std::env::var("FINCH_TEST_BRAIN_LISTENER_BACKUP_FD").as_deref() == Ok("110")
                && std::env::var("FINCH_TEST_DAEMON_LISTENER_BACKUP_FD").as_deref() == Ok("111"),
            "sealed TCP authority does not match supervisor-owned listeners"
        );
        restore_supervisor_listener(110, 10, &brain_addr)?;
        restore_supervisor_listener(111, 11, &daemon_addr)?;
    }
    anyhow::ensure!(
        std::env::var_os("HOME").as_deref() == Some(home.as_os_str())
            && std::env::var_os("FINCH_BRAIN_TEST_HOME").as_deref() == Some(home.as_os_str())
            && std::env::var_os("FINCH_BRAIN_TEST_ROOT").as_deref() == Some(root.as_os_str()),
        "wrapper proof does not match the active HOME and Brain root"
    );
    use sha2::Digest as _;
    let environment_password = std::env::var("FINCH_TEST_BRAIN_PASSWORD").unwrap_or_default();
    anyhow::ensure!(
        std::env::var("FINCH_TEST_BRAIN_ADDR").unwrap_or_default() == brain_addr
            && std::env::var("FINCH_TEST_DAEMON_ADDR").unwrap_or_default() == daemon_addr
            && hex::encode(sha2::Sha256::digest(environment_password.as_bytes()))
                == password_digest
            && std::env::var_os("FINCH_TEST_IPC_SOCKET").as_deref() == Some(ipc_socket.as_os_str()),
        "live endpoint environment does not match the parent-sealed authority"
    );
    anyhow::ensure!(
        std::env::var_os("FINCH_TEST_SOCKET_ROOT").as_deref() == Some(socket_root.as_os_str())
            && ipc_socket == socket_root.join("daemon.sock")
            && socket_root.is_absolute()
            && socket_root.canonicalize()? == socket_root
            && !std::fs::symlink_metadata(&socket_root)?
                .file_type()
                .is_symlink(),
        "wrapper proof does not match the supervisor-owned socket root"
    );
    anyhow::ensure!(
        home.is_absolute()
            && root == home.join(".finch/brains")
            && home.canonicalize()? == home
            && root.canonicalize()? == root
            && !std::fs::symlink_metadata(&home)?.file_type().is_symlink()
            && !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
        "wrapper proof paths are not the active disposable filesystem objects"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let home_metadata = std::fs::metadata(&home)?;
        let root_metadata = std::fs::metadata(&root)?;
        let socket_root_metadata = std::fs::metadata(&socket_root)?;
        anyhow::ensure!(
            home_identity == format!("{}:{}", home_metadata.dev(), home_metadata.ino())
                && root_identity == format!("{}:{}", root_metadata.dev(), root_metadata.ino()),
            "wrapper proof filesystem identity no longer matches HOME and Brain root"
        );
        anyhow::ensure!(
            socket_root_identity
                == format!(
                    "{}:{}",
                    socket_root_metadata.dev(),
                    socket_root_metadata.ino()
                )
                && socket_root_metadata.uid() == nix::unistd::geteuid().as_raw()
                && socket_root_metadata.mode() & 0o777 == 0o700
                && socket_root_metadata.nlink() >= 1,
            "wrapper proof socket-root identity changed"
        );
    }
    let parse_identity = |identity: &str| -> anyhow::Result<(u64, u64)> {
        let (device, inode) = identity
            .split_once(':')
            .context("wrapper proof has an invalid filesystem identity")?;
        Ok((device.parse()?, inode.parse()?))
    };
    Ok(IsolatedTestProof {
        home,
        root,
        home_identity: parse_identity(home_identity)?,
        root_identity: parse_identity(root_identity)?,
        brain_addr,
        daemon_addr,
        ipc_socket,
        socket_root,
        socket_root_identity: parse_identity(socket_root_identity)?,
        supervisor_pid,
        password_digest: password_digest.to_owned(),
    })
}

#[doc(hidden)]
pub fn isolated_test_proof_if_present() -> anyhow::Result<Option<IsolatedTestProof>> {
    let present = std::env::var_os("FINCH_BRAIN_TEST_ISOLATED").is_some()
        || std::env::var_os("FINCH_BRAIN_TEST_PROOF_FD").is_some()
        || std::env::var_os("FINCH_TEST_SUPERVISOR_PID").is_some();
    if !present {
        return Ok(None);
    }
    isolated_test_proof().map(Some)
}

#[cfg(all(test, unix))]
pub(crate) fn supervised_test_subprocess_command() -> std::process::Command {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;

    let proof = isolated_test_proof().expect("subprocess requires valid supervisor authority");
    let proof_raw = unsafe { nix::libc::fcntl(9, nix::libc::F_DUPFD_CLOEXEC, 200) };
    assert!(
        proof_raw >= 0,
        "could not duplicate supervisor proof descriptor"
    );
    let proof_fd = unsafe { OwnedFd::from_raw_fd(proof_raw) };
    let brain_listener = proof.duplicate_brain_listener().unwrap();
    let daemon_listener = proof.duplicate_daemon_listener().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in [
                (proof_fd.as_raw_fd(), 9),
                (brain_listener.as_raw_fd(), 10),
                (daemon_listener.as_raw_fd(), 11),
                (brain_listener.as_raw_fd(), 110),
                (daemon_listener.as_raw_fd(), 111),
            ] {
                if nix::libc::dup2(source, target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if nix::libc::fcntl(target, nix::libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command
}

#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IsolatedTestSocketIdentity {
    device: u64,
    inode: u64,
}

#[cfg(all(test, unix))]
pub(crate) fn validate_isolated_test_socket(
    proof: &IsolatedTestProof,
    path: &std::path::Path,
) -> anyhow::Result<IsolatedTestSocketIdentity> {
    use anyhow::Context as _;
    use nix::fcntl::{open, openat, AtFlags, OFlag};
    use nix::sys::stat::Mode;
    use nix::sys::stat::{fstat, fstatat, SFlag};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    anyhow::ensure!(path.is_absolute(), "test IPC socket must be absolute");
    let parent = path.parent().context("test IPC socket has no parent")?;
    let relative = parent
        .strip_prefix(&proof.socket_root)
        .context("test IPC socket is outside the supervisor-owned socket root")?;
    anyhow::ensure!(
        relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "test IPC socket must be inside the supervisor-owned socket root"
    );
    let name = path
        .file_name()
        .context("test IPC socket has no final component")?;
    let home_raw = open(
        &proof.socket_root,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(home_raw) };
    let home_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        (home_stat.st_dev as u64, home_stat.st_ino as u64) == proof.socket_root_identity,
        "test IPC socket-root identity changed"
    );
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            anyhow::bail!("test IPC parent contains an unsafe component");
        };
        let raw = openat(
            Some(directory.as_raw_fd()),
            component,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        directory = unsafe { std::fs::File::from_raw_fd(raw) };
    }
    let directory_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(directory_stat.st_mode).contains(SFlag::S_IFDIR)
            && directory_stat.st_uid == nix::unistd::geteuid().as_raw()
            && directory_stat.st_nlink >= 1,
        "test IPC parent is not an owned directory"
    );
    let socket_stat = fstatat(
        Some(directory.as_raw_fd()),
        name,
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(socket_stat.st_mode).contains(SFlag::S_IFSOCK)
            && socket_stat.st_uid == nix::unistd::geteuid().as_raw()
            && socket_stat.st_nlink == 1,
        "test IPC path is not an owned, unlinked-name Unix socket"
    );
    Ok(IsolatedTestSocketIdentity {
        device: socket_stat.st_dev as u64,
        inode: socket_stat.st_ino as u64,
    })
}

#[cfg(all(test, unix))]
pub(crate) fn authenticate_isolated_test_peer(
    stream: &tokio::net::UnixStream,
) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd as _;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    let peer_pid = {
        let mut credential: nix::libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<nix::libc::ucred>() as nix::libc::socklen_t;
        let result = unsafe {
            nix::libc::getsockopt(
                fd,
                nix::libc::SOL_SOCKET,
                nix::libc::SO_PEERCRED,
                (&mut credential as *mut nix::libc::ucred).cast(),
                &mut length,
            )
        };
        anyhow::ensure!(result == 0, "test IPC peer credentials are unavailable");
        credential.pid
    };
    #[cfg(target_os = "macos")]
    let peer_pid = {
        let mut pid: nix::libc::pid_t = 0;
        let mut length = std::mem::size_of::<nix::libc::pid_t>() as nix::libc::socklen_t;
        let result = unsafe {
            nix::libc::getsockopt(
                fd,
                nix::libc::SOL_LOCAL,
                nix::libc::LOCAL_PEERPID,
                (&mut pid as *mut nix::libc::pid_t).cast(),
                &mut length,
            )
        };
        anyhow::ensure!(result == 0, "test IPC peer credentials are unavailable");
        pid
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("test IPC peer authentication is unsupported on this platform");
    anyhow::ensure!(
        isolated_test_peer_process_is_owned(peer_pid),
        "test IPC peer is outside the supervisor-owned process group"
    );
    Ok(())
}

#[cfg(all(test, unix))]
fn isolated_test_peer_process_is_owned(peer_pid: nix::libc::pid_t) -> bool {
    let peer_group = unsafe { nix::libc::getpgid(peer_pid) };
    peer_pid > 0 && peer_group > 0 && peer_group == unsafe { nix::libc::getpgrp() }
}

#[cfg(all(test, unix))]
mod isolation_tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn isolated_listener_validation_rejects_bound_non_listening_socket_and_restores_flags() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        isolated_test_proof().unwrap();
        let raw = unsafe { nix::libc::socket(nix::libc::AF_INET, nix::libc::SOCK_STREAM, 0) };
        assert!(raw >= 0);
        let socket = unsafe { std::net::TcpListener::from_raw_fd(raw) };
        let address = nix::libc::sockaddr_in {
            sin_len: std::mem::size_of::<nix::libc::sockaddr_in>() as u8,
            sin_family: nix::libc::AF_INET as u8,
            sin_port: 0,
            sin_addr: nix::libc::in_addr {
                s_addr: u32::from_ne_bytes([127, 0, 0, 1]),
            },
            sin_zero: [0; 8],
        };
        assert_eq!(
            unsafe {
                nix::libc::bind(
                    raw,
                    (&address as *const nix::libc::sockaddr_in).cast(),
                    std::mem::size_of_val(&address) as nix::libc::socklen_t,
                )
            },
            0
        );
        let expected = socket.local_addr().unwrap().to_string();
        let original_flags = unsafe { nix::libc::fcntl(socket.as_raw_fd(), nix::libc::F_GETFL) };
        assert!(original_flags >= 0);
        assert!(duplicate_validated_listener(socket.as_raw_fd(), &expected).is_err());
        assert_eq!(
            unsafe { nix::libc::fcntl(socket.as_raw_fd(), nix::libc::F_GETFL) },
            original_flags,
            "listener challenge did not restore the original file status flags"
        );
    }

    #[test]
    fn isolated_proof_rejects_self_issued_environment_authority() {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        const MODE_ENV: &str = "FINCH_TEST_FORGED_PROOF_MODE";
        if let Ok(mode) = std::env::var(MODE_ENV) {
            let valid = isolated_test_proof().unwrap();
            if mode == "swapped-low-listener" {
                assert_eq!(unsafe { nix::libc::dup2(11, 10) }, 10);
                let repaired = isolated_test_proof().unwrap();
                assert_eq!(
                    repaired
                        .duplicate_brain_listener()
                        .unwrap()
                        .local_addr()
                        .unwrap()
                        .to_string(),
                    repaired.brain_addr
                );
                return;
            }
            if mode == "swapped-backup-listener" {
                assert_eq!(unsafe { nix::libc::dup2(111, 110) }, 110);
                assert!(isolated_test_proof().is_err());
                return;
            }
            let path = valid.home.join(format!("forged-proof-{mode}"));
            let mut writer = std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            if mode == "self" {
                let token = "self-issued";
                let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
                let executable_metadata = std::fs::metadata(&executable).unwrap();
                writeln!(writer, "{token}").unwrap();
                writeln!(writer, "{}", valid.home.display()).unwrap();
                writeln!(writer, "{}", valid.root.display()).unwrap();
                writeln!(
                    writer,
                    "{}:{}",
                    valid.home_identity.0, valid.home_identity.1
                )
                .unwrap();
                writeln!(
                    writer,
                    "{}:{}",
                    valid.root_identity.0, valid.root_identity.1
                )
                .unwrap();
                writeln!(writer, "{}", valid.brain_addr).unwrap();
                writeln!(writer, "{}", valid.daemon_addr).unwrap();
                writeln!(writer, "{}", valid.password_digest).unwrap();
                writeln!(writer, "{}", valid.ipc_socket.display()).unwrap();
                writeln!(writer, "{}", valid.socket_root.display()).unwrap();
                writeln!(
                    writer,
                    "{}:{}",
                    valid.socket_root_identity.0, valid.socket_root_identity.1
                )
                .unwrap();
                writeln!(writer, "{}", std::process::id()).unwrap();
                writeln!(writer, "{}", executable.display()).unwrap();
                writeln!(
                    writer,
                    "{}:{}",
                    executable_metadata.dev(),
                    executable_metadata.ino()
                )
                .unwrap();
                writer.sync_all().unwrap();
                writer
                    .set_permissions(std::fs::Permissions::from_mode(0o400))
                    .unwrap();
                drop(writer);
                let reader = std::fs::OpenOptions::new().read(true).open(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
                assert_eq!(unsafe { nix::libc::dup2(reader.as_raw_fd(), 9) }, 9);
                std::env::set_var("FINCH_BRAIN_TEST_TOKEN", token);
                std::env::set_var("FINCH_TEST_SUPERVISOR_PID", std::process::id().to_string());
            } else {
                std::fs::remove_file(&path).unwrap();
                assert_eq!(unsafe { nix::libc::dup2(writer.as_raw_fd(), 9) }, 9);
            }
            assert!(isolated_test_proof().is_err());
            return;
        }

        for mode in [
            "self",
            "writable",
            "swapped-low-listener",
            "swapped-backup-listener",
        ] {
            let status = supervised_test_subprocess_command()
                .args([
                    "--exact",
                    "brain::isolation_tests::isolated_proof_rejects_self_issued_environment_authority",
                    "--nocapture",
                ])
                .env(MODE_ENV, mode)
                .status()
                .unwrap();
            assert!(status.success(), "{mode} proof rejection subprocess failed");
        }
    }

    #[test]
    fn isolated_socket_validation_accepts_owned_socket_by_descriptor() {
        let proof = isolated_test_proof().unwrap();
        let socket = proof.socket_root.join("accept.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        validate_isolated_test_socket(&proof, &socket).unwrap();
    }

    #[test]
    fn isolated_socket_validation_rejects_default_socket_symlink() {
        let proof = isolated_test_proof().unwrap();
        let outside = proof.socket_root.join("outside.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&outside).unwrap();
        let socket = proof.ipc_socket.clone();
        std::os::unix::fs::symlink(&outside, &socket).unwrap();
        assert!(validate_isolated_test_socket(&proof, &socket).is_err());
    }

    #[test]
    fn isolated_socket_validation_detects_identity_swap_across_connect_window() {
        let proof = isolated_test_proof().unwrap();
        let socket = proof.socket_root.join("swap.sock");
        let first = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let before = validate_isolated_test_socket(&proof, &socket).unwrap();
        drop(first);
        std::fs::remove_file(&socket).unwrap();
        let _replacement = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let after = validate_isolated_test_socket(&proof, &socket).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn isolated_peer_authentication_rejects_connected_outside_process() {
        use std::os::unix::process::CommandExt as _;
        const SOCKET_ENV: &str = "FINCH_TEST_OUTSIDER_SOCKET";
        const READY_ENV: &str = "FINCH_TEST_OUTSIDER_READY";
        const ACCEPTED_ENV: &str = "FINCH_TEST_OUTSIDER_ACCEPTED";
        if let (Some(socket), Some(ready), Some(accepted)) = (
            std::env::var_os(SOCKET_ENV),
            std::env::var_os(READY_ENV),
            std::env::var_os(ACCEPTED_ENV),
        ) {
            let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
            std::fs::write(ready, b"ready").unwrap();
            let _connection = listener.accept().unwrap();
            std::fs::write(accepted, b"accepted").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
            return;
        }

        let proof = isolated_test_proof().unwrap();
        let socket = proof.socket_root.join("outsider.sock");
        let original_name = proof.socket_root.join("original.sock");
        let original_listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let before = validate_isolated_test_socket(&proof, &socket).unwrap();
        std::fs::rename(&socket, &original_name).unwrap();
        let ready = proof.home.join("outsider-ready");
        let accepted = proof.home.join("outsider-accepted");
        let mut outsider = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "brain::isolation_tests::isolated_peer_authentication_rejects_connected_outside_process",
                "--nocapture",
            ])
            .env(SOCKET_ENV, &socket)
            .env(READY_ENV, &ready)
            .env(ACCEPTED_ENV, &accepted)
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "outsider did not bind"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let stream = runtime
            .block_on(tokio::net::UnixStream::connect(&socket))
            .unwrap();
        while !accepted.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "outsider did not accept"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_file(&socket).unwrap();
        std::fs::rename(&original_name, &socket).unwrap();
        let after = validate_isolated_test_socket(&proof, &socket).unwrap();
        assert_eq!(before, after, "fixture must reproduce an A-to-B-to-A swap");
        let result = authenticate_isolated_test_peer(&stream);
        assert!(result.is_err());
        drop(stream);
        drop(original_listener);
        outsider.kill().unwrap();
        outsider.wait().unwrap();
    }
}
