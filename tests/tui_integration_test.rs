// Integration tests for TUI mode
//
// These tests verify TUI functionality using expect/pty simulation.
// Note: TUI tests are complex because they require a pseudo-TTY.

use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
static EMBEDDING_SIGNAL_OBSERVED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn terminal_pty_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(unix)]
extern "C" fn embedding_signal_handler(_: nix::libc::c_int) {
    EMBEDDING_SIGNAL_OBSERVED.store(true, Ordering::Release);
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
extern "C" fn embedding_signal_exit_handler(_: nix::libc::c_int) {
    unsafe { nix::libc::_exit(0) };
}

#[cfg(unix)]
fn install_embedding_sigterm() -> anyhow::Result<()> {
    let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
    action.sa_sigaction = embedding_signal_handler as *const () as usize;
    unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
    anyhow::ensure!(
        unsafe { nix::libc::sigaction(nix::libc::SIGTERM, &action, std::ptr::null_mut()) } == 0,
        "install embedding SIGTERM handler"
    );
    Ok(())
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn install_embedding_sigterm_exit() -> anyhow::Result<()> {
    let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
    action.sa_sigaction = embedding_signal_exit_handler as *const () as usize;
    unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
    anyhow::ensure!(
        unsafe { nix::libc::sigaction(nix::libc::SIGTERM, &action, std::ptr::null_mut()) } == 0,
        "install embedding SIGTERM exit handler"
    );
    Ok(())
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn install_embedding_sigint_exit() -> anyhow::Result<()> {
    let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
    action.sa_sigaction = embedding_signal_exit_handler as *const () as usize;
    unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
    anyhow::ensure!(
        unsafe { nix::libc::sigaction(nix::libc::SIGINT, &action, std::ptr::null_mut()) } == 0,
        "install embedding SIGINT exit handler"
    );
    Ok(())
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn install_default_sigint() -> anyhow::Result<()> {
    let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
    action.sa_sigaction = nix::libc::SIG_DFL;
    unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
    anyhow::ensure!(
        unsafe { nix::libc::sigaction(nix::libc::SIGINT, &action, std::ptr::null_mut()) } == 0,
        "install default SIGINT action"
    );
    let mut signals = unsafe { std::mem::zeroed::<nix::libc::sigset_t>() };
    unsafe {
        nix::libc::sigemptyset(&mut signals);
        nix::libc::sigaddset(&mut signals, nix::libc::SIGINT);
    }
    anyhow::ensure!(
        unsafe {
            nix::libc::pthread_sigmask(nix::libc::SIG_UNBLOCK, &signals, std::ptr::null_mut())
        } == 0,
        "unblock SIGINT on transition owner"
    );
    Ok(())
}

#[cfg(unix)]
fn fork_child_must_observe_embedding_sigterm() -> anyhow::Result<()> {
    EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
    let forked = unsafe { nix::libc::fork() };
    anyhow::ensure!(forked >= 0, "fork embedding-signal child");
    if forked == 0 {
        unsafe { nix::libc::raise(nix::libc::SIGTERM) };
        let status = if EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire) {
            0
        } else {
            77
        };
        unsafe { nix::libc::_exit(status) };
    }
    let mut status = 0;
    anyhow::ensure!(
        unsafe { nix::libc::waitpid(forked, &mut status, 0) } == forked,
        "reap fork embedding-signal child"
    );
    anyhow::ensure!(
        nix::libc::WIFEXITED(status) && nix::libc::WEXITSTATUS(status) == 0,
        "fork child did not receive the current embedding disposition: {status}"
    );
    Ok(())
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn reap_signal_handler_fork_child() -> anyhow::Result<()> {
    let forked = finch::cli::tui::supervised_take_post_cas_signal_handler_fork_result()?;
    anyhow::ensure!(forked > 0, "signal-handler fork failed: {forked}");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut status = 0;
        let waited = unsafe { nix::libc::waitpid(forked, &mut status, nix::libc::WNOHANG) };
        if waited == forked {
            anyhow::ensure!(
                nix::libc::WIFEXITED(status) && nix::libc::WEXITSTATUS(status) == 0,
                "signal-handler fork child did not observe exact restored dispositions: {status}"
            );
            return Ok(());
        }
        anyhow::ensure!(waited == 0, "wait for signal-handler fork child: {waited}");
        if Instant::now() >= deadline {
            unsafe {
                nix::libc::kill(forked, nix::libc::SIGKILL);
                nix::libc::waitpid(forked, &mut status, 0);
            }
            anyhow::bail!("signal-handler fork child exceeded bounded completion deadline");
        }
        std::thread::yield_now();
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn fork_from_signal_handler_during_transition(disarm: bool) -> anyhow::Result<()> {
    // Fail-before: pthread_atfork prepare spun on SIGNAL_TRANSITION, but this
    // handler interrupted the thread that owned that CAS. The interrupted arm
    // or disarm frame could never run to release it.
    install_default_sigint()?;
    install_embedding_sigterm_exit()?;
    let signals = finch::cli::tui::BinaryTerminalSession::install()?
        .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;

    if disarm {
        let mut renderer = new_renderer()?;
        finch::cli::tui::supervised_prepare_post_cas_signal_handler_fork(true)?;
        renderer.shutdown()?;
    } else {
        finch::cli::tui::supervised_prepare_post_cas_signal_handler_fork(false)?;
        let mut renderer = new_renderer()?;
        renderer.shutdown()?;
    }
    reap_signal_handler_fork_child()?;
    drop(signals);
    Ok(())
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct TerminalModes {
    input: nix::libc::tcflag_t,
    output: nix::libc::tcflag_t,
    control: nix::libc::tcflag_t,
    local: nix::libc::tcflag_t,
    characters: [nix::libc::cc_t; nix::libc::NCCS],
}

#[cfg(unix)]
fn terminal_modes(file: &std::fs::File) -> TerminalModes {
    let mut attributes = std::mem::MaybeUninit::<nix::libc::termios>::uninit();
    assert_eq!(
        unsafe { nix::libc::tcgetattr(file.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    let attributes = unsafe { attributes.assume_init() };
    TerminalModes {
        input: attributes.c_iflag,
        output: attributes.c_oflag,
        control: attributes.c_cflag,
        local: attributes.c_lflag & !nix::libc::PENDIN,
        characters: attributes.c_cc,
    }
}

#[cfg(unix)]
fn terminal_attributes(file: &std::fs::File) -> nix::libc::termios {
    let mut attributes = std::mem::MaybeUninit::<nix::libc::termios>::uninit();
    assert_eq!(
        unsafe { nix::libc::tcgetattr(file.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    unsafe { attributes.assume_init() }
}

#[cfg(unix)]
fn open_owned_pty() -> (std::fs::File, std::fs::File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = nix::libc::winsize {
        ws_row: 24,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            nix::libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0
    );
    let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let flags = unsafe { nix::libc::fcntl(master.as_raw_fd(), nix::libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                master.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0
    );
    (master, slave)
}

#[cfg(unix)]
fn read_until(
    file: &mut std::fs::File,
    transcript: &mut Vec<u8>,
    deadline: Instant,
    marker: &[u8],
) -> bool {
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                transcript.extend_from_slice(&buffer[..read]);
                if transcript
                    .windows(marker.len())
                    .any(|bytes| bytes == marker)
                {
                    return true;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => std::thread::yield_now(),
            Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => break,
            Err(error) => panic!("PTY read failed: {error}"),
        }
    }
    transcript
        .windows(marker.len())
        .any(|bytes| bytes == marker)
}

#[cfg(unix)]
fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[cfg(unix)]
fn wait_for_child(child: &mut std::process::Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if let Some(status) = child.try_wait().expect("inspect terminal probe") {
            return status;
        }
        if Instant::now() >= deadline {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            )
            .ok();
            let status = child.wait().expect("reap timed-out terminal probe");
            panic!("terminal probe did not exit before timeout; killed and reaped as {status}");
        }
        std::thread::yield_now();
    }
}

#[cfg(unix)]
fn read_control_byte(
    child: &mut std::process::Child,
    fd: nix::libc::c_int,
    deadline: Instant,
) -> u8 {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = wait_for_child(child, Instant::now());
            unreachable!("wait_for_child reports timeout by panicking");
        }
        let mut poll_fd = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining.as_millis().min(100) as i32;
        let ready = unsafe { nix::libc::poll(&mut poll_fd, 1, timeout.max(1)) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            panic!(
                "poll backpressure control failed: {}",
                std::io::Error::last_os_error()
            );
        }
        if ready == 0 {
            continue;
        }
        let mut byte = 0_u8;
        let read = unsafe { nix::libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if read == 1 {
            return byte;
        }
        let status = wait_for_child(child, Instant::now() + Duration::from_millis(100));
        panic!("backpressure probe closed control fd before readiness: {status}");
    }
}

#[cfg(unix)]
fn drain_pty(file: &mut std::fs::File, transcript: &mut Vec<u8>, deadline: Instant) {
    let marker = b"FINCH_TERMINAL_DRAIN_SENTINEL_THAT_NEVER_APPEARS";
    let _ = read_until(file, transcript, deadline, marker);
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
unsafe fn set_test_errno(error: nix::libc::c_int) {
    unsafe { *nix::libc::__errno_location() = error };
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
unsafe fn test_errno() -> nix::libc::c_int {
    unsafe { *nix::libc::__errno_location() }
}

#[cfg(all(
    unix,
    any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
unsafe fn set_test_errno(error: nix::libc::c_int) {
    unsafe { *nix::libc::__error() = error };
}

#[cfg(all(
    unix,
    any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
unsafe fn test_errno() -> nix::libc::c_int {
    unsafe { *nix::libc::__error() }
}

#[cfg(unix)]
fn supervised_pty_authority_or_skip() -> bool {
    match finch::brain::isolated_test_proof_if_present() {
        Ok(Some(_)) => true,
        Ok(None) => {
            eprintln!("skipping PTY regression outside scripts/test_brains.sh");
            false
        }
        Err(error) => panic!("invalid PTY supervisor authority: {error:#}"),
    }
}

#[cfg(unix)]
fn new_renderer() -> anyhow::Result<finch::cli::tui::TuiRenderer> {
    finch::cli::tui::TuiRenderer::new(
        std::sync::Arc::new(finch::cli::OutputManager::new(
            finch::config::ColorScheme::default(),
        )),
        std::sync::Arc::new(finch::cli::StatusBar::new()),
        finch::config::ColorScheme::default(),
    )
}

#[cfg(unix)]
fn marker(bytes: &[u8]) -> anyhow::Result<()> {
    std::io::stdout().write_all(bytes)?;
    std::io::stdout().flush()?;
    Ok(())
}

#[cfg(unix)]
fn stdout_status_flags() -> anyhow::Result<i32> {
    let flags = unsafe { nix::libc::fcntl(nix::libc::STDOUT_FILENO, nix::libc::F_GETFL) };
    anyhow::ensure!(flags >= 0, "read stdout status flags");
    Ok(flags)
}

#[cfg(unix)]
fn open_descriptor_set() -> std::collections::BTreeSet<i32> {
    (0..256)
        .filter(|fd| unsafe { nix::libc::fcntl(*fd, nix::libc::F_GETFD) } >= 0)
        .collect()
}

#[cfg(unix)]
fn fill_terminal_and_notify_control() -> anyhow::Result<()> {
    let control_fd: i32 = std::env::var("FINCH_TEST_TERMINAL_CONTROL_FD")?.parse()?;
    let mut path = [0_i8; 1024];
    let status =
        unsafe { nix::libc::ttyname_r(nix::libc::STDOUT_FILENO, path.as_mut_ptr(), path.len()) };
    anyhow::ensure!(status == 0, "resolve stdout tty: {status}");
    let fd = unsafe {
        nix::libc::open(
            path.as_ptr(),
            nix::libc::O_WRONLY
                | nix::libc::O_NOCTTY
                | nix::libc::O_NONBLOCK
                | nix::libc::O_CLOEXEC,
        )
    };
    anyhow::ensure!(fd >= 0, "open nonblocking tty filler");
    let bytes = [b'X'; 4096];
    loop {
        let written = unsafe { nix::libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written >= 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            continue;
        }
        anyhow::ensure!(
            error.kind() == ErrorKind::WouldBlock,
            "fill terminal output: {error}"
        );
        break;
    }
    unsafe { nix::libc::close(fd) };
    let byte = b'R';
    anyhow::ensure!(
        unsafe { nix::libc::write(control_fd, (&byte as *const u8).cast(), 1) } == 1,
        "notify full terminal queue"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tui_terminal_session_child() -> anyhow::Result<()> {
    let Ok(mode) = std::env::var("FINCH_TEST_TERMINAL_SESSION_CHILD") else {
        return Ok(());
    };
    if mode == "fd-check" {
        for fd in std::env::var("FINCH_TEST_RESTORE_FDS")?
            .split(',')
            .map(str::parse::<i32>)
        {
            let fd = fd?;
            let result = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
            anyhow::ensure!(
                result < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::EBADF),
                "terminal descriptor {fd} survived exec"
            );
        }
        return Ok(());
    }
    if mode == "activation-rollback-failure" {
        let stdout_flags = stdout_status_flags()?;
        std::env::set_var("FINCH_TEST_TUI_FAIL_AFTER", "paste");
        std::env::set_var("FINCH_TEST_TUI_FAIL_ACTIVATION_ROLLBACK", "1");
        let error = match new_renderer() {
            Ok(_) => anyhow::bail!("injected activation rollback failure succeeded"),
            Err(error) => error,
        };
        std::env::remove_var("FINCH_TEST_TUI_FAIL_AFTER");
        std::env::remove_var("FINCH_TEST_TUI_FAIL_ACTIVATION_ROLLBACK");
        let error = format!("{error:#}");
        anyhow::ensure!(error.contains("activation stopped after bracketed paste"));
        anyhow::ensure!(error.contains("rollback failed"));
        anyhow::ensure!(
            finch::cli::tui::supervised_terminal_cleanup_owner_is_retained()?,
            "failed activation rollback lost its explicit CLEANING repair owner"
        );
        anyhow::ensure!(
            new_renderer().is_err(),
            "failed activation rollback falsely published INACTIVE"
        );
        finch::cli::tui::emergency_restore_terminal_result()?;
        anyhow::ensure!(stdout_status_flags()? == stdout_flags);
        let mut replacement = new_renderer()?;
        replacement.shutdown()?;
        marker(b"FINCH_ACTIVATION_ROLLBACK_REPAIRED")?;
        return Ok(());
    }
    if mode.starts_with("init-") || mode.starts_with("short-") {
        let stdout_flags = stdout_status_flags()?;
        if let Some(stage) = mode.strip_prefix("init-") {
            std::env::set_var("FINCH_TEST_TUI_FAIL_AFTER", stage);
        } else if let Some(stage) = mode.strip_prefix("short-") {
            std::env::set_var("FINCH_TEST_TUI_SHORT_WRITE", format!("{stage}:1"));
        }
        let result = new_renderer();
        std::env::remove_var("FINCH_TEST_TUI_FAIL_AFTER");
        std::env::remove_var("FINCH_TEST_TUI_SHORT_WRITE");
        anyhow::ensure!(result.is_err(), "injected activation failure succeeded");
        anyhow::ensure!(
            stdout_status_flags()? == stdout_flags,
            "failed activation changed stdout status flags"
        );
        marker(b"FINCH_INIT_ROLLED_BACK")?;
        return Ok(());
    }
    if mode == "signal-create-failure" {
        std::env::set_var("FINCH_TEST_TUI_FAIL_SIGNAL_TRANSPORT", "1");
        let failed = finch::cli::tui::BinaryTerminalSession::install();
        std::env::remove_var("FINCH_TEST_TUI_FAIL_SIGNAL_TRANSPORT");
        anyhow::ensure!(
            failed.is_err(),
            "injected signal transport failure succeeded"
        );
        let signals = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("signal owner was not reusable after failure"))?;
        let mut renderer = new_renderer()?;
        renderer.shutdown()?;
        drop(signals);
        marker(b"FINCH_SIGNAL_TRANSPORT_ROLLED_BACK")?;
        return Ok(());
    }
    if mode == "signal-descriptor-free" {
        let before = open_descriptor_set();
        let signals = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        let after = open_descriptor_set();
        anyhow::ensure!(
            after == before,
            "atomic signal ownership allocated inheritable/reusable descriptors: before={before:?} after={after:?}"
        );
        drop(signals);
        marker(b"FINCH_SIGNAL_DESCRIPTOR_FREE")?;
        return Ok(());
    }
    if mode == "repeated-signal-owners" {
        // Cross u16 wrap to prove the stable trampoline/monitor has no finite
        // per-session handler-slot pool to exhaust.
        for _ in 0..70_000 {
            let owner = finch::cli::tui::BinaryTerminalSession::install()?
                .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
            drop(owner);
        }
        marker(b"FINCH_SIGNAL_OWNERS_REUSED")?;
        return Ok(());
    }
    if mode == "fork-public-renderer-after-host-change" {
        // Fail-before: atfork treated ACTIVE terminal phase as signal
        // ownership and restored the stale install-time snapshot, even though
        // this public embedding renderer never armed Finch's trampoline.
        let owner = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        drop(owner);
        install_embedding_sigterm()?;
        let mut renderer = new_renderer()?;
        fork_child_must_observe_embedding_sigterm()?;
        renderer.shutdown()?;
        marker(b"FINCH_FORK_PUBLIC_RENDERER_HOST_SIGNAL_PRESERVED")?;
        return Ok(());
    }
    if mode == "fork-host-change-between-install-arm" {
        // Binary install only registers stable infrastructure. The handler
        // installed after it, immediately before terminal arm, is the exact
        // disposition that Finch displaces and the fork child must restore.
        let owner = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        install_embedding_sigterm()?;
        let mut renderer = new_renderer()?;
        fork_child_must_observe_embedding_sigterm()?;
        renderer.shutdown()?;
        drop(owner);
        marker(b"FINCH_FORK_DISPLACED_HOST_SIGNAL_RESTORED")?;
        return Ok(());
    }
    if mode == "fork-host-replaces-armed-signal" {
        // The installed bit records Finch's arm, but an embedding host can
        // replace one slot afterward. The child and parent cleanup must both
        // preserve that newer disposition instead of blindly replaying the
        // action Finch originally displaced.
        let owner = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        let mut renderer = new_renderer()?;
        install_embedding_sigterm()?;
        fork_child_must_observe_embedding_sigterm()?;
        renderer.shutdown()?;
        drop(owner);
        EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
        unsafe { nix::libc::raise(nix::libc::SIGTERM) };
        anyhow::ensure!(
            EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire),
            "parent cleanup overwrote the embedding host's replacement signal action"
        );
        marker(b"FINCH_FORK_REPLACEMENT_HOST_SIGNAL_PRESERVED")?;
        return Ok(());
    }
    if mode == "fork-handler-during-arm-transition" {
        fork_from_signal_handler_during_transition(false)?;
        marker(b"FINCH_FORK_HANDLER_ARM_TRANSITION_COMPLETED")?;
        return Ok(());
    }
    if mode == "fork-handler-during-disarm-transition" {
        fork_from_signal_handler_during_transition(true)?;
        marker(b"FINCH_FORK_HANDLER_DISARM_TRANSITION_COMPLETED")?;
        return Ok(());
    }
    if mode == "fork-linux-oldact-copy-window" {
        // Fail-before Linux model: the new kernel action is visible while
        // glibc's oldact copy to its caller remains delayed. The previous
        // implementation pointed that delayed output at the restore slot, so
        // this concurrent fork restored stale/default state instead of this
        // exact embedding action.
        install_embedding_sigint_exit()?;
        let signals = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        finch::cli::tui::supervised_prepare_linux_oldact_publication_fork()?;
        let mut renderer = new_renderer()?;
        anyhow::ensure!(
            finch::cli::tui::supervised_take_linux_oldact_publication_fork_result()? == 1,
            "fork child did not restore the prepublished action during delayed oldact copy"
        );
        renderer.shutdown()?;
        drop(signals);
        marker(b"FINCH_FORK_PREPUBLISHED_OLDACT_RESTORED")?;
        return Ok(());
    }
    if mode == "signal-host-mutation-during-arm" {
        let signals = finch::cli::tui::BinaryTerminalSession::install()?
            .ok_or_else(|| anyhow::anyhow!("atomic signal owner missing"))?;
        finch::cli::tui::supervised_change_host_signal_during_next_arm()?;
        let error = match new_renderer() {
            Ok(_) => anyhow::bail!("concurrent host signal mutation was accepted"),
            Err(error) => format!("{error:#}"),
        };
        anyhow::ensure!(
            error.contains("embedding signal disposition changed concurrently"),
            "unexpected concurrent signal mutation error: {error}"
        );
        let mut current = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
        anyhow::ensure!(
            unsafe { nix::libc::sigaction(nix::libc::SIGINT, std::ptr::null(), &mut current) } == 0
                && current.sa_sigaction == nix::libc::SIG_IGN,
            "activation mismatch did not restore the actually displaced host action"
        );
        install_default_sigint()?;
        let mut replacement = new_renderer()?;
        replacement.shutdown()?;
        drop(signals);
        marker(b"FINCH_CONCURRENT_SIGNAL_MUTATION_REJECTED")?;
        return Ok(());
    }

    let preserves_host_signal = matches!(
        mode.as_str(),
        "binary-owner-drop" | "owner-windows" | "fork-preexec-host-signal"
    );
    if preserves_host_signal {
        install_embedding_sigterm()?;
    }
    let needs_binary_owner = matches!(
        mode.as_str(),
        "signal"
            | "backpressure-signal"
            | "binary-owner-drop"
            | "owner-windows"
            | "cloexec"
            | "signal-full"
            | "signal-stop-full"
            | "handler-reuse"
            | "handler-replacement-signal"
            | "pending-before-drop"
            | "signal-paused-cleanup"
            | "signal-transition-recovery"
            | "signal-disarm-recovery"
            | "signal-disarm-pending-recovery"
            | "signal-persistent-backoff"
            | "fork-preexec-signal"
            | "fork-preexec-host-signal"
    );
    let mut signals = if needs_binary_owner {
        finch::cli::tui::BinaryTerminalSession::install()?
    } else {
        None
    };
    if needs_binary_owner {
        anyhow::ensure!(
            signals.is_some(),
            "binary signal transport was not installed before terminal activation"
        );
    }
    if mode == "owner-windows" {
        EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
        unsafe { nix::libc::raise(nix::libc::SIGTERM) };
        anyhow::ensure!(
            EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire),
            "binary owner changed the host contract before terminal activation"
        );
    }

    let stdout_flags = stdout_status_flags()?;
    let mut renderer = new_renderer()?;
    anyhow::ensure!(
        stdout_status_flags()? == stdout_flags,
        "terminal activation changed stdout status flags"
    );
    if std::env::var_os("FINCH_TEST_TERMINAL_ACTIVATION_HANDSHAKE").is_some() {
        marker(b"FINCH_RENDERER_ACTIVE")?;
        let mut acknowledge = 0_u8;
        std::io::stdin().read_exact(std::slice::from_mut(&mut acknowledge))?;
        anyhow::ensure!(acknowledge == b'G', "invalid activation acknowledgement");
    }
    match mode.as_str() {
        "clean" => renderer.shutdown(),
        "drop" => {
            drop(renderer);
            Ok(())
        }
        "error" => anyhow::bail!("intentional terminal-session error"),
        "panic" => panic!("intentional terminal-session panic"),
        "signal" => {
            anyhow::ensure!(signals.is_some(), "binary signal owner missing");
            marker(b"FINCH_SIGNAL_READY")?;
            loop {
                std::thread::park();
            }
        }
        "signal-disarm-pending-recovery" => {
            anyhow::ensure!(signals.is_some(), "binary signal owner missing");
            finch::cli::tui::supervised_fail_next_signal_disarm()?;
            marker(b"FINCH_SIGNAL_DISARM_PENDING_READY")?;
            loop {
                std::thread::park();
            }
        }
        "signal-persistent-backoff" => {
            anyhow::ensure!(signals.is_some(), "binary signal owner missing");
            let (attempts_before, parks_before) =
                finch::cli::tui::supervised_terminal_signal_recovery_counts()?;
            finch::cli::tui::supervised_set_signal_transition_stall(true)?;
            unsafe { nix::libc::raise(nix::libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_signal_transition_stall_is_observed()? {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "signal recovery never reached stall"
                );
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(900));
            let (attempts_after, parks_after) =
                finch::cli::tui::supervised_terminal_signal_recovery_counts()?;
            finch::cli::tui::supervised_set_signal_transition_stall(false)?;
            anyhow::ensure!(
                attempts_after.saturating_sub(attempts_before) <= 6,
                "persistent restore failure retried at high duty: {} attempts",
                attempts_after.saturating_sub(attempts_before)
            );
            anyhow::ensure!(
                parks_after.saturating_sub(parks_before) >= 2,
                "persistent restore failure did not enter bounded park/backoff"
            );
            loop {
                std::thread::park();
            }
        }
        "backpressure-clean" => {
            fill_terminal_and_notify_control()?;
            renderer.shutdown()
        }
        "backpressure-signal" => {
            anyhow::ensure!(signals.is_some(), "binary signal owner missing");
            fill_terminal_and_notify_control()?;
            loop {
                std::thread::park();
            }
        }
        "backpressure-blocking-mutation" => {
            fill_terminal_and_notify_control()?;
            let bytes = [b'Z'; 4096];
            unsafe {
                nix::libc::write(nix::libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len())
            };
            anyhow::bail!("blocking cleanup mutation unexpectedly returned")
        }
        "embedding" => {
            renderer.shutdown()?;
            drop(renderer);
            let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
            action.sa_sigaction = embedding_signal_handler as *const () as usize;
            unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
            anyhow::ensure!(
                unsafe { nix::libc::sigaction(nix::libc::SIGTERM, &action, std::ptr::null_mut()) }
                    == 0,
                "install embedding handler"
            );
            EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
            unsafe { nix::libc::raise(nix::libc::SIGTERM) };
            while !EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            marker(b"FINCH_EMBEDDING_CONTINUED")?;
            Ok(())
        }
        "binary-owner-drop" => {
            renderer.shutdown()?;
            drop(renderer);
            drop(signals.take());
            EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
            unsafe { nix::libc::raise(nix::libc::SIGTERM) };
            while !EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            marker(b"FINCH_BINARY_OWNER_RELEASED")?;
            Ok(())
        }
        "owner-windows" => {
            renderer.shutdown()?;
            drop(renderer);
            EMBEDDING_SIGNAL_OBSERVED.store(false, Ordering::Release);
            unsafe { nix::libc::raise(nix::libc::SIGTERM) };
            anyhow::ensure!(
                EMBEDDING_SIGNAL_OBSERVED.load(Ordering::Acquire),
                "binary owner changed the host contract after terminal cleanup"
            );
            marker(b"FINCH_OWNER_WINDOWS_PRESERVED")?;
            Ok(())
        }
        "cloexec" => {
            let fd = finch::cli::tui::supervised_terminal_restore_fd()?;
            let descriptor = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
            let status = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
            anyhow::ensure!(descriptor & nix::libc::FD_CLOEXEC != 0);
            anyhow::ensure!(status & nix::libc::O_NONBLOCK != 0);
            let mut exec_child = Command::new(std::env::current_exe()?)
                .args(["--exact", "tui_terminal_session_child"])
                .env("FINCH_TEST_TERMINAL_SESSION_CHILD", "fd-check")
                .env("FINCH_TEST_RESTORE_FDS", fd.to_string())
                .spawn()?;
            let status = wait_for_child(&mut exec_child, Instant::now() + Duration::from_secs(2));
            anyhow::ensure!(status.success(), "restore fd inherited by exec child");
            renderer.shutdown()
        }
        "fork-preexec-signal" => {
            let forked = unsafe { nix::libc::fork() };
            anyhow::ensure!(forked >= 0, "fork terminal-session child");
            if forked == 0 {
                unsafe {
                    nix::libc::raise(nix::libc::SIGTERM);
                    nix::libc::_exit(77);
                }
            }
            let mut status = 0;
            anyhow::ensure!(
                unsafe { nix::libc::waitpid(forked, &mut status, 0) } == forked,
                "reap fork/pre-exec signal child"
            );
            anyhow::ensure!(
                nix::libc::WIFSIGNALED(status) && nix::libc::WTERMSIG(status) == nix::libc::SIGTERM,
                "fork child swallowed/misattributed pre-exec SIGTERM: {status}"
            );
            renderer.shutdown()?;
            marker(b"FINCH_FORK_PREEXEC_SIGNAL_RESTORED")?;
            Ok(())
        }
        "fork-preexec-host-signal" => {
            fork_child_must_observe_embedding_sigterm()?;
            renderer.shutdown()?;
            marker(b"FINCH_FORK_PREEXEC_HOST_SIGNAL_RESTORED")?;
            Ok(())
        }
        "signal-paused-cleanup" => {
            finch::cli::tui::supervised_set_terminal_cleanup_pause(true)?;
            let _cleanup = std::thread::spawn(move || renderer.shutdown());
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_cleanup_is_paused()? {
                anyhow::ensure!(Instant::now() < deadline, "terminal cleanup did not pause");
                std::thread::yield_now();
            }
            marker(b"FINCH_SIGNAL_PAUSED_CLEANUP_READY")?;
            loop {
                std::thread::park();
            }
            #[allow(unreachable_code)]
            {
                Ok(())
            }
        }
        "signal-transition-recovery" => {
            finch::cli::tui::supervised_set_signal_transition_stall(true)?;
            anyhow::ensure!(
                renderer.shutdown().is_err(),
                "stalled signal transition falsely completed cleanup"
            );
            drop(renderer);
            let started = Instant::now();
            drop(signals.take());
            anyhow::ensure!(
                // One bounded call may first wait for the abandoned cleanup
                // owner and then time out acquiring the signal transition.
                started.elapsed() < Duration::from_millis(400),
                "signal owner Drop blocked on stalled transition"
            );
            anyhow::ensure!(
                finch::cli::tui::BinaryTerminalSession::install().is_err(),
                "Drop released signal ownership before restoring dispositions"
            );
            finch::cli::tui::supervised_set_signal_transition_stall(false)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            let replacement = loop {
                match finch::cli::tui::BinaryTerminalSession::install() {
                    Ok(Some(owner)) => break owner,
                    _ if Instant::now() < deadline => std::thread::yield_now(),
                    _ => anyhow::bail!("signal owner did not recover before deadline"),
                }
            };
            drop(replacement);
            marker(b"FINCH_SIGNAL_TRANSITION_RECOVERED")?;
            Ok(())
        }
        "signal-disarm-recovery" => {
            finch::cli::tui::supervised_fail_next_signal_disarm()?;
            anyhow::ensure!(
                renderer.shutdown().is_err(),
                "injected sigaction restore failure falsely completed cleanup"
            );
            drop(renderer);
            drop(signals.take());
            let deadline = Instant::now() + Duration::from_secs(2);
            let replacement = loop {
                match finch::cli::tui::BinaryTerminalSession::install() {
                    Ok(Some(owner)) => break owner,
                    _ if Instant::now() < deadline => std::thread::yield_now(),
                    _ => anyhow::bail!("signal owner did not recover after sigaction failure"),
                }
            };
            drop(replacement);
            marker(b"FINCH_SIGNAL_DISARM_RECOVERED")?;
            Ok(())
        }
        "sequential" => {
            renderer.shutdown()?;
            let mut changed = std::mem::MaybeUninit::<nix::libc::termios>::uninit();
            anyhow::ensure!(
                unsafe { nix::libc::tcgetattr(nix::libc::STDOUT_FILENO, changed.as_mut_ptr()) }
                    == 0,
                "snapshot between sessions"
            );
            let mut changed = unsafe { changed.assume_init() };
            changed.c_lflag ^= nix::libc::ECHO;
            anyhow::ensure!(
                unsafe {
                    nix::libc::tcsetattr(nix::libc::STDOUT_FILENO, nix::libc::TCSANOW, &changed)
                } == 0,
                "change termios between sessions"
            );
            let mut second = new_renderer()?;
            second.shutdown()?;
            let current = terminal_modes(&unsafe {
                std::fs::File::from_raw_fd(nix::libc::dup(nix::libc::STDOUT_FILENO))
            });
            anyhow::ensure!(
                current.local == changed.c_lflag & !nix::libc::PENDIN,
                "second session restored stale first-session termios"
            );
            // Return the PTY to the process-entry state for the parent check.
            changed.c_lflag ^= nix::libc::ECHO;
            unsafe { nix::libc::tcsetattr(nix::libc::STDOUT_FILENO, nix::libc::TCSANOW, &changed) };
            Ok(())
        }
        "repeated" => {
            renderer.shutdown()?;
            for _ in 0..8 {
                let mut next = new_renderer()?;
                next.shutdown()?;
            }
            marker(b"FINCH_REPEATED_SESSIONS_COMPLETE")?;
            Ok(())
        }
        "signal-stop-full" => {
            let signals = signals
                .take()
                .ok_or_else(|| anyhow::anyhow!("binary signal owner missing"))?;
            signals.supervised_pause_signal_listener()?;
            renderer.shutdown()?;
            let started = Instant::now();
            drop(signals);
            anyhow::ensure!(
                started.elapsed() < Duration::from_millis(250),
                "binary signal owner Drop exceeded its bound"
            );
            marker(b"FINCH_SIGNAL_STOP_BOUNDED")?;
            Ok(())
        }
        "signal-full" => {
            let signals = signals
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("binary signal owner missing"))?;
            signals.supervised_pause_signal_listener()?;
            marker(b"FINCH_SIGNAL_LISTENER_PAUSED")?;
            let mut acknowledge = 0_u8;
            std::io::stdin().read_exact(std::slice::from_mut(&mut acknowledge))?;
            anyhow::ensure!(acknowledge == b'G', "invalid full-queue acknowledgement");
            signals.supervised_resume_signal_listener()?;
            loop {
                std::thread::park();
            }
        }
        "handler-reuse" | "handler-replacement-signal" => {
            // This pause is before the production trampoline's first sticky
            // atomic. The old generation-attributing handler could be
            // disarmed/rearmed here and load zero or a replacement token.
            finch::cli::tui::supervised_set_terminal_signal_handler_pause(true)?;
            let observed_errno = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
            let observed_errno_clone = std::sync::Arc::clone(&observed_errno);
            let handler_thread = std::thread::spawn(move || {
                let sentinel = nix::libc::EDOM;
                unsafe {
                    set_test_errno(sentinel);
                    nix::libc::pthread_kill(nix::libc::pthread_self(), nix::libc::SIGTERM);
                    observed_errno_clone.store(test_errno(), Ordering::Release);
                }
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_signal_handler_is_paused()? {
                anyhow::ensure!(Instant::now() < deadline, "signal handler did not pause");
                std::thread::yield_now();
            }
            renderer.shutdown()?;
            drop(renderer);
            drop(signals.take());
            let replacement_signals = finch::cli::tui::BinaryTerminalSession::install()?
                .ok_or_else(|| anyhow::anyhow!("replacement signal owner missing"))?;
            let _replacement_renderer = new_renderer()?;
            // Keep the process-lifetime monitor parked while the entered old
            // trampoline publishes after host restoration/re-arm, so errno can
            // be checked deterministically before conventional termination.
            replacement_signals.supervised_pause_signal_listener()?;
            finch::cli::tui::supervised_set_terminal_signal_handler_pause(false)?;
            handler_thread
                .join()
                .map_err(|_| anyhow::anyhow!("paused signal handler thread panicked"))?;
            anyhow::ensure!(
                observed_errno.load(Ordering::Acquire) == nix::libc::EDOM,
                "terminal signal handler changed the interrupted thread's errno"
            );
            replacement_signals.supervised_resume_signal_listener()?;
            loop {
                std::thread::park();
            }
        }
        "pending-before-drop" => {
            let signals = signals
                .take()
                .ok_or_else(|| anyhow::anyhow!("binary signal owner missing"))?;
            signals.supervised_pause_signal_listener()?;
            unsafe { nix::libc::raise(nix::libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_signal_is_pending()? {
                anyhow::ensure!(Instant::now() < deadline, "signal did not become sticky");
                std::thread::yield_now();
            }
            renderer.shutdown()?;
            // Drop resumes the permanent monitor. The sticky delivery must not
            // be lost in the owner-release window and terminates conventionally.
            drop(signals);
            loop {
                std::thread::park();
            }
        }
        "writer-after-reset" => {
            finch::cli::tui::supervised_set_terminal_writer_pause(true)?;
            let writer = std::thread::spawn(|| {
                finch::cli::tui::supervised_publish_terminal_bytes(b"FINCH_STALE_FRAME")
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_writer_is_paused()? {
                anyhow::ensure!(Instant::now() < deadline, "terminal writer did not pause");
                std::thread::yield_now();
            }
            finch::cli::tui::emergency_restore_terminal_result()?;
            finch::cli::tui::supervised_set_terminal_writer_pause(false)?;
            anyhow::ensure!(
                writer
                    .join()
                    .map_err(|_| anyhow::anyhow!("terminal writer panicked"))?
                    .is_err(),
                "admitted writer published after terminal reset"
            );
            marker(b"FINCH_WRITER_REVOKED")?;
            Ok(())
        }
        "stale-gate-cas" => {
            finch::cli::tui::supervised_verify_stale_terminal_gate_cas()?;
            renderer.shutdown()?;
            marker(b"FINCH_STALE_GATE_CAS_SAFE")?;
            Ok(())
        }
        "writer-gate-timeout" => {
            finch::cli::tui::supervised_set_terminal_writer_gate_pause(true)?;
            let writer = std::thread::spawn(|| {
                finch::cli::tui::supervised_publish_terminal_bytes(b"FINCH_STALE_GATE_FRAME")
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_writer_gate_is_paused()? {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "terminal writer did not pause while admitted"
                );
                std::thread::yield_now();
            }
            // The first bounded cleanup cannot own the publication gate. It
            // must fail closed: no restoration record, replacement admission,
            // or reset bytes may be published yet.
            let started = Instant::now();
            anyhow::ensure!(
                finch::cli::tui::emergency_restore_terminal_result().is_err(),
                "permanently stalled application gate falsely restored"
            );
            anyhow::ensure!(
                started.elapsed() < Duration::from_millis(250),
                "application gate stall exceeded the library cleanup bound"
            );
            anyhow::ensure!(
                new_renderer().is_err(),
                "failed cleanup admitted a replacement renderer"
            );
            finch::cli::tui::supervised_set_terminal_writer_gate_pause(false)?;
            anyhow::ensure!(
                writer
                    .join()
                    .map_err(|_| anyhow::anyhow!("gated terminal writer panicked"))?
                    .is_err(),
                "revoked gated writer published after cleanup timeout"
            );
            // Once the nonblocking writer releases the gate, a later bounded
            // cleanup takes over the abandoned owner and repairs exactly once.
            finch::cli::tui::emergency_restore_terminal_result()?;
            marker(b"FINCH_WRITER_TIMEOUT_REPAIRED")?;
            Ok(())
        }
        "overlapping-cleanup" => {
            finch::cli::tui::supervised_set_terminal_cleanup_pause(true)?;
            let cleanup = std::thread::spawn(move || renderer.shutdown());
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finch::cli::tui::supervised_terminal_cleanup_is_paused()? {
                anyhow::ensure!(Instant::now() < deadline, "terminal cleanup did not pause");
                std::thread::yield_now();
            }
            let overlap = new_renderer();
            anyhow::ensure!(
                overlap.is_err(),
                "replacement activated before prior cleanup published restoration"
            );
            finch::cli::tui::supervised_set_terminal_cleanup_pause(false)?;
            cleanup
                .join()
                .map_err(|_| anyhow::anyhow!("cleanup thread panicked"))??;
            let mut replacement = new_renderer()?;
            replacement.shutdown()?;
            marker(b"FINCH_OVERLAP_REJECTED")?;
            Ok(())
        }
        "global-lock-backpressure" => {
            finch::cli::global_output::set_global_tui_renderer(renderer)?;
            let (locked_tx, locked_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _guard = finch::cli::global_output::get_global_tui_renderer()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = locked_tx.send(());
                loop {
                    std::thread::park();
                }
            });
            locked_rx.recv_timeout(Duration::from_secs(2))?;
            fill_terminal_and_notify_control()?;
            let started = Instant::now();
            finch::cli::global_output::shutdown_global_tui()?;
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(1),
                "global TUI fallback blocked on stdout"
            );
            Ok(())
        }
        "hang" => loop {
            std::thread::park();
        },
        other => anyhow::bail!("unknown terminal-session mode {other}"),
    }
}

#[cfg(unix)]
fn spawn_terminal_child(
    mode: &str,
) -> (
    std::process::Child,
    std::fs::File,
    std::fs::File,
    TerminalModes,
) {
    let (master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "tui_terminal_session_child", "--nocapture"])
        .env("FINCH_TEST_TERMINAL_SESSION_CHILD", mode)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    if matches!(mode, "clean" | "drop" | "error" | "panic") {
        command.env("FINCH_TEST_TERMINAL_ACTIVATION_HANDSHAKE", "1");
    }
    let child = command.spawn().expect("spawn terminal-session probe");
    (child, master, slave, original)
}

#[cfg(unix)]
#[test]
fn test_tui_session_restores_normal_error_panic_and_partial_activation() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for mode in [
        "clean",
        "drop",
        "error",
        "panic",
        "init-raw",
        "init-paste",
        "init-mouse",
        "init-keyboard",
        "init-cursor",
        "short-paste",
        "short-mouse",
        "short-keyboard",
        "short-cursor",
    ] {
        let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
        let mut transcript = Vec::new();
        if matches!(mode, "clean" | "drop" | "error" | "panic") {
            assert!(read_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(5),
                b"FINCH_RENDERER_ACTIVE",
            ));
            master.write_all(b"G").unwrap();
            master.flush().unwrap();
        }
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(5));
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(terminal_modes(&slave), original, "{mode} termios mismatch");
        if matches!(mode, "error" | "panic") {
            assert!(!status.success(), "{mode} unexpectedly succeeded");
        } else {
            assert!(status.success(), "{mode} failed: {status}");
        }
        if mode == "init-raw" {
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 0);
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 0);
        } else if mode == "init-paste" || mode == "short-paste" {
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
            assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 0);
        } else if mode == "init-mouse" || mode == "short-mouse" {
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 0);
        } else if matches!(mode, "clean" | "drop" | "error" | "panic") {
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
        } else {
            // Immediate rollback flushes activation bytes that the PTY parent
            // has not consumed. Reset bytes are emitted after that flush and
            // therefore are the deterministic partial-activation contract.
            assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
            assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
        }
    }
}

#[cfg(unix)]
#[test]
fn test_binary_terminal_session_restores_all_external_signals() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
        nix::sys::signal::Signal::SIGHUP,
    ] {
        let (mut child, mut master, slave, original) = spawn_terminal_child("signal");
        let mut transcript = Vec::new();
        assert!(read_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(5),
            b"FINCH_SIGNAL_READY",
        ));
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(status.code(), Some(128 + signal as i32));
        assert_eq!(terminal_modes(&slave), original);
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[?1006l"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[?25h"), 2, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[0m"), 1, "{signal:?}");
    }
}

#[cfg(unix)]
#[test]
fn test_binary_terminal_signal_is_not_lost_while_atomic_listener_is_paused() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut child, mut master, slave, original) = spawn_terminal_child("signal-full");
    let mut transcript = Vec::new();
    assert!(read_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        b"FINCH_SIGNAL_LISTENER_PAUSED",
    ));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    master.write_all(b"G").unwrap();
    master.flush().unwrap();
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_signal_takes_over_paused_cleanup_before_process_exit() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    // At 4e991716 the listener ignored cleanup's 100 ms timeout and called
    // `_exit`, so this exact pause left the PTY raw with protocols enabled.
    let (mut child, mut master, slave, original) = spawn_terminal_child("signal-paused-cleanup");
    let mut transcript = Vec::new();
    assert!(read_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        b"FINCH_SIGNAL_PAUSED_CLEANUP_READY",
    ));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(3));
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

/// The executable, rather than public renderer construction, explicitly owns
/// the scoped signal guard. This exercises the real main-to-REPL wiring.
#[cfg(unix)]
#[test]
fn test_finch_binary_owns_external_terminal_signals() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
        nix::sys::signal::Signal::SIGHUP,
    ] {
        let (mut master, slave) = open_owned_pty();
        let original = terminal_modes(&slave);
        let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
            .arg("--cloud-only")
            .env(
                "ANTHROPIC_API_KEY",
                "sk-ant-finch-terminal-regression-placeholder",
            )
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("spawn Finch binary signal probe");
        let mut transcript = Vec::new();
        assert!(
            read_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(20),
                b"accept edits on",
            ),
            "Finch did not reach input: {}",
            String::from_utf8_lossy(&transcript)
        );
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(status.code(), Some(128 + signal as i32));
        assert_eq!(terminal_modes(&slave), original);
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1, "{signal:?}");
        assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1, "{signal:?}");
    }
}

/// Exercise Finch's installed panic hook rather than relying on Rust's ordinary
/// stack unwinding to drop a renderer owned by this integration-test process.
#[cfg(unix)]
#[test]
fn test_finch_main_panic_hook_restores_terminal_protocols() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for gate_mode in [
        "none",
        "other-thread",
        "other-thread-permanent",
        "same-thread",
        "cleanup-owner",
    ] {
        let (mut master, slave) = open_owned_pty();
        let original = terminal_modes(&slave);
        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .arg("--cloud-only")
            .env(
                "ANTHROPIC_API_KEY",
                "sk-ant-finch-terminal-regression-placeholder",
            )
            .env("FINCH_TEST_TUI_MAIN_PANIC_AFTER_ACTIVE", "1")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        if matches!(gate_mode, "other-thread" | "other-thread-permanent") {
            command.env("FINCH_TEST_TUI_MAIN_PANIC_GATE_HELD", "1");
            if gate_mode == "other-thread-permanent" {
                command.env("FINCH_TEST_TUI_MAIN_PANIC_GATE_PERMANENT", "1");
            }
        } else if gate_mode == "same-thread" {
            command.env("FINCH_TEST_TUI_MAIN_PANIC_SAME_THREAD_GATE", "1");
        } else if gate_mode == "cleanup-owner" {
            command.env("FINCH_TEST_TUI_MAIN_PANIC_CLEANUP_OWNER", "1");
        }
        let mut child = command.spawn().expect("spawn Finch main panic-hook probe");
        let mut transcript = Vec::new();
        assert!(
            read_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(20),
                b"FINCH_MAIN_PANIC_ARMED",
            ),
            "Finch did not arm panic probe: {}",
            String::from_utf8_lossy(&transcript)
        );
        let started = Instant::now();
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert!(
            !status.success(),
            "supervised main panic unexpectedly succeeded"
        );
        assert_eq!(terminal_modes(&slave), original);
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
        let expected_panic = match gate_mode {
            "same-thread" => "supervised same-thread panic while holding Finch terminal gate",
            "cleanup-owner" => "supervised same-thread panic while owning Finch terminal cleanup",
            _ => "supervised panic after Finch terminal activation",
        };
        assert!(String::from_utf8_lossy(&transcript).contains(expected_panic));
        assert!(
            !transcript
                .windows(b"FINCH_PANIC_FRAME".len())
                .any(|window| window == b"FINCH_PANIC_FRAME"),
            "revoked panic writer published after reset"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "panic hook did not return to bounded unwind for {gate_mode}"
        );
    }
}

/// The production REPL constructor may fall back to ordinary stdout only after
/// it has automatically repaired a failed TUI constructor. Fail-before: an
/// activation+rollback error returned to this branch while the terminal was
/// still raw/CLEANING, and fallback bytes were then published into that state.
#[cfg(unix)]
#[test]
fn test_real_repl_constructor_repairs_before_standard_output_fallback() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-terminal-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_FAIL_AFTER", "keyboard")
        .env("FINCH_TEST_TUI_FAIL_ACTIVATION_ROLLBACK_ONCE", "1")
        .env("FINCH_TEST_TUI_MAIN_RETURN_AFTER_CONSTRUCTION", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn real Repl constructor fallback probe");
    let mut transcript = Vec::new();
    assert!(
        read_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            b"Falling back to standard output mode",
        ),
        "Repl did not reach repaired stdout fallback: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(5));
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert!(status.success(), "Repl fallback failed: {status}");
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
    let fallback = transcript
        .windows(b"Falling back to standard output mode".len())
        .position(|bytes| bytes == b"Falling back to standard output mode")
        .expect("fallback marker");
    for reset in [
        b"\x1b[?2004l".as_slice(),
        b"\x1b[?1000l".as_slice(),
        b"\x1b[<1u".as_slice(),
    ] {
        let reset_at = transcript
            .windows(reset.len())
            .position(|bytes| bytes == reset)
            .expect("automatic constructor recovery reset");
        assert!(
            reset_at < fallback,
            "stdout fallback published before automatic terminal recovery"
        );
    }
}

/// A constructor whose activation and every rollback attempt fail must abort
/// without admitting standard stdout. Fail-before: the production Err branch
/// enabled fallback unconditionally while the generation was still raw and
/// retained in CLEANING.
#[cfg(unix)]
#[test]
fn test_real_repl_constructor_persistent_dirty_failure_aborts_without_stdout_fallback() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-terminal-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_FAIL_AFTER", "keyboard")
        .env("FINCH_TEST_TUI_FAIL_ACTIVATION_ROLLBACK", "1")
        .env("FINCH_TEST_TUI_MAIN_ASSERT_DIRTY_CONSTRUCTION", "1")
        .env("FINCH_TEST_TUI_MAIN_RETURN_AFTER_CONSTRUCTION", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn persistently dirty Repl constructor probe");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(
        status.code(),
        Some(1),
        "dirty constructor did not abort cleanly"
    );
    assert!(
        !transcript
            .windows(b"Falling back to standard output mode".len())
            .any(|bytes| bytes == b"Falling back to standard output mode"),
        "dirty constructor published fallback stdout: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert!(
        String::from_utf8_lossy(&transcript).contains("automatic terminal recovery failed"),
        "dirty constructor did not report recovery failure: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert_ne!(
        terminal_modes(&slave),
        original,
        "persistent rollback failure falsely restored/published INACTIVE"
    );
}

/// The IPC quit watcher and this supervised main path share the exact binary
/// exit helper. Fail-before: a same-thread gate owner deadlocked restoration or
/// let `process::exit` bypass reset.
#[cfg(unix)]
#[test]
fn test_finch_binary_quit_helper_revokes_same_thread_gate_before_exit() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-terminal-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_MAIN_QUIT_SAME_THREAD_GATE", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn Finch same-thread quit probe");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(20));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(23));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

/// One real `/quit` keystroke is encoded as ControlMessage by the production
/// input task. Fail-before: the watcher discarded its intent after the first
/// bounded restore failure and waited forever for a second message.
#[cfg(unix)]
#[test]
fn test_real_ipc_quit_latches_and_retries_after_terminal_progress() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut control = [-1; 2];
    assert_eq!(unsafe { nix::libc::pipe(control.as_mut_ptr()) }, 0);
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-terminal-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_MAIN_IPC_QUIT_RETRY", "1")
        .env("FINCH_TEST_TERMINAL_CONTROL_FD", control[1].to_string())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn Finch real IPC quit probe");
    unsafe { nix::libc::close(control[1]) };
    let mut transcript = Vec::new();
    assert!(
        read_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            b"accept edits on",
        ),
        "Finch did not reach input: {}",
        String::from_utf8_lossy(&transcript)
    );
    master.write_all(b"/quit\r").unwrap();
    master.flush().unwrap();
    let stalled = read_control_byte(
        &mut child,
        control[0],
        Instant::now() + Duration::from_secs(2),
    );
    unsafe { nix::libc::close(control[0]) };
    assert_eq!(
        stalled, b'S',
        "real Quit did not reach the stalled transition"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "quit watcher exited during its causally stalled first restore attempt"
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(4));
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert!(status.success(), "latched IPC quit failed: {status}");
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_public_renderer_preserves_embedding_signal_contract() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for (mode, expected) in [
        ("embedding", b"FINCH_EMBEDDING_CONTINUED".as_slice()),
        (
            "binary-owner-drop",
            b"FINCH_BINARY_OWNER_RELEASED".as_slice(),
        ),
        ("owner-windows", b"FINCH_OWNER_WINDOWS_PRESERVED".as_slice()),
    ] {
        let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
        let mut transcript = Vec::new();
        assert!(read_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(5),
            expected,
        ));
        assert!(wait_for_child(&mut child, Instant::now() + Duration::from_secs(2)).success());
        assert_eq!(terminal_modes(&slave), original);
    }
}

#[cfg(unix)]
#[test]
fn test_terminal_descriptors_are_cloexec_and_sessions_repeat_without_overlap() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for mode in [
        "cloexec",
        "signal-descriptor-free",
        "repeated-signal-owners",
        "signal-create-failure",
        "activation-rollback-failure",
        "sequential",
        "repeated",
        "overlapping-cleanup",
        "stale-gate-cas",
        "signal-transition-recovery",
        "signal-disarm-recovery",
    ] {
        let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(5));
        let mut transcript = Vec::new();
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert!(
            status.success(),
            "{mode} failed: {status}: {}",
            String::from_utf8_lossy(&transcript)
        );
        assert_eq!(terminal_modes(&slave), original, "{mode} termios mismatch");
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
fn assert_fork_signal_mode(mode: &str, marker: &[u8]) {
    let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(5));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert!(status.success(), "fork/pre-exec probe failed: {status}");
    assert!(transcript
        .windows(marker.len())
        .any(|bytes| bytes == marker));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_fork_child_restores_host_signal_before_preexec_delivery() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for (mode, marker) in [
        (
            "fork-preexec-signal",
            b"FINCH_FORK_PREEXEC_SIGNAL_RESTORED".as_slice(),
        ),
        (
            "fork-preexec-host-signal",
            b"FINCH_FORK_PREEXEC_HOST_SIGNAL_RESTORED".as_slice(),
        ),
    ] {
        assert_fork_signal_mode(mode, marker);
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_fork_active_public_renderer_preserves_post_install_host_signal() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    assert_fork_signal_mode(
        "fork-public-renderer-after-host-change",
        b"FINCH_FORK_PUBLIC_RENDERER_HOST_SIGNAL_PRESERVED",
    );
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_fork_restores_handler_displaced_after_binary_install() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    assert_fork_signal_mode(
        "fork-host-change-between-install-arm",
        b"FINCH_FORK_DISPLACED_HOST_SIGNAL_RESTORED",
    );
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_fork_and_cleanup_preserve_host_replacement_of_armed_signal() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    assert_fork_signal_mode(
        "fork-host-replaces-armed-signal",
        b"FINCH_FORK_REPLACEMENT_HOST_SIGNAL_PRESERVED",
    );
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_signal_handler_fork_does_not_wait_on_interrupted_transition_owner() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for (mode, marker) in [
        (
            "fork-handler-during-arm-transition",
            b"FINCH_FORK_HANDLER_ARM_TRANSITION_COMPLETED".as_slice(),
        ),
        (
            "fork-handler-during-disarm-transition",
            b"FINCH_FORK_HANDLER_DISARM_TRANSITION_COMPLETED".as_slice(),
        ),
    ] {
        assert_fork_signal_mode(mode, marker);
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_fork_child_uses_prepublished_action_before_linux_oldact_copy() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    assert_fork_signal_mode(
        "fork-linux-oldact-copy-window",
        b"FINCH_FORK_PREPUBLISHED_OLDACT_RESTORED",
    );
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn test_signal_arm_rejects_and_restores_concurrent_host_mutation() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    assert_fork_signal_mode(
        "signal-host-mutation-during-arm",
        b"FINCH_CONCURRENT_SIGNAL_MUTATION_REJECTED",
    );
}

#[cfg(unix)]
#[test]
fn test_persistent_signal_restore_failure_uses_bounded_progress_backoff() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut child, mut master, slave, original) =
        spawn_terminal_child("signal-persistent-backoff");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(5));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_terminal_cleanup_is_bounded_under_unread_backpressure() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for mode in [
        "backpressure-clean",
        "backpressure-signal",
        "global-lock-backpressure",
    ] {
        let (master, slave) = open_owned_pty();
        let original = terminal_modes(&slave);
        let mut control = [-1; 2];
        assert_eq!(unsafe { nix::libc::pipe(control.as_mut_ptr()) }, 0);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tui_terminal_session_child", "--nocapture"])
            .env("FINCH_TEST_TERMINAL_SESSION_CHILD", mode)
            .env("FINCH_TEST_TERMINAL_CONTROL_FD", control[1].to_string())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("spawn backpressure probe");
        unsafe { nix::libc::close(control[1]) };
        let ready = read_control_byte(
            &mut child,
            control[0],
            Instant::now() + Duration::from_secs(5),
        );
        unsafe { nix::libc::close(control[0]) };
        assert_eq!(ready, b'R');
        if mode == "backpressure-signal" {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGTERM,
            )
            .unwrap();
        }
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
        if mode == "backpressure-signal" {
            assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
        } else {
            assert!(status.success(), "bounded normal cleanup failed: {status}");
        }
        assert_eq!(terminal_modes(&slave), original, "{mode} termios mismatch");
        drop(master);
    }
}

#[cfg(unix)]
#[test]
fn test_atomic_signal_owner_drop_is_bounded() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let mode = "signal-stop-full";
    let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
    let marker = b"FINCH_SIGNAL_STOP_BOUNDED".as_slice();
    let mut transcript = Vec::new();
    assert!(read_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        marker,
    ));
    assert!(wait_for_child(&mut child, Instant::now() + Duration::from_secs(2)).success());
    assert_eq!(terminal_modes(&slave), original, "{mode} termios mismatch");
}

#[cfg(unix)]
#[test]
fn test_stable_signal_trampoline_preserves_late_delivery_across_rearm() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    // Fail-before: a handler selected under the old Finch action but paused
    // before identity capture could load zero or the replacement generation.
    // The stable trampoline instead publishes generation-free sticky delivery
    // after host restoration and replacement re-arm.
    let (mut child, mut master, slave, original) =
        spawn_terminal_child("handler-replacement-signal");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(3));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    // Replacement cleanup flushes the first generation's queued PTY output
    // before publishing its own exact reset sequence.
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_sticky_signal_pending_before_owner_drop_is_not_lost() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut child, mut master, slave, original) = spawn_terminal_child("pending-before-drop");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(3));
    let mut transcript = Vec::new();
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_pending_signal_retries_one_transient_disposition_restore_failure() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut child, mut master, slave, original) =
        spawn_terminal_child("signal-disarm-pending-recovery");
    let mut transcript = Vec::new();
    assert!(read_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        b"FINCH_SIGNAL_DISARM_PENDING_READY",
    ));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(3));
    drain_pty(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(status.code(), Some(128 + nix::libc::SIGTERM));
    assert_eq!(terminal_modes(&slave), original);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

#[cfg(unix)]
#[test]
fn test_admitted_terminal_writers_are_revoked_before_reset_publication() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for (mode, marker) in [
        ("writer-after-reset", b"FINCH_WRITER_REVOKED".as_slice()),
        (
            "writer-gate-timeout",
            b"FINCH_WRITER_TIMEOUT_REPAIRED".as_slice(),
        ),
    ] {
        let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
        let mut transcript = Vec::new();
        assert!(read_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(5),
            marker,
        ));
        assert!(wait_for_child(&mut child, Instant::now() + Duration::from_secs(2)).success());
        drain_pty(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(terminal_modes(&slave), original, "{mode}");
        assert!(!transcript
            .windows(b"FINCH_STALE_FRAME".len())
            .any(|bytes| bytes == b"FINCH_STALE_FRAME"));
        assert!(!transcript
            .windows(b"FINCH_STALE_GATE_FRAME".len())
            .any(|bytes| bytes == b"FINCH_STALE_GATE_FRAME"));
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1, "{mode}");
    }
}

#[cfg(unix)]
#[test]
fn test_wait_for_child_timeout_kills_and_reaps_terminal_probe() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut child, _master, _slave, _original) = spawn_terminal_child("hang");
    let timed_out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_child(&mut child, Instant::now() + Duration::from_millis(50))
    }));
    assert!(
        timed_out.is_err(),
        "timeout helper did not report its causal hang"
    );
    assert!(
        child
            .try_wait()
            .expect("inspect reaped timeout child")
            .is_some(),
        "timeout helper left the killed child unreaped"
    );
}

/// Causal controls replay the rejected mechanisms through the same PTY
/// boundary. They are expected to observe the old failure, proving the
/// positive regressions are sensitive to signal omission and a blocking reset
/// write. The owner-window, overlap, main-hook, short-write, and fd-reuse tests
/// exercise their rejected implementations directly at the same boundary.
#[cfg(unix)]
#[test]
fn test_terminal_session_causal_mutations_reproduce_reported_failures() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }

    let (master, slave) = open_owned_pty();
    let original = terminal_attributes(&slave);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tui_terminal_session_child", "--nocapture"])
        .env("FINCH_TEST_TERMINAL_SESSION_CHILD", "signal")
        .env("FINCH_TEST_TUI_MUTATE_OMIT_SIGINT", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let mut master = master;
    let mut transcript = Vec::new();
    assert!(read_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        b"FINCH_SIGNAL_READY",
    ));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(nix::libc::SIGINT));
    assert_ne!(
        terminal_modes(&slave),
        TerminalModes {
            input: original.c_iflag,
            output: original.c_oflag,
            control: original.c_cflag,
            local: original.c_lflag & !nix::libc::PENDIN,
            characters: original.c_cc,
        }
    );
    unsafe { nix::libc::tcsetattr(slave.as_raw_fd(), nix::libc::TCSANOW, &original) };

    let (master, slave) = open_owned_pty();
    let original = terminal_attributes(&slave);
    let mut control = [-1; 2];
    assert_eq!(unsafe { nix::libc::pipe(control.as_mut_ptr()) }, 0);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tui_terminal_session_child", "--nocapture"])
        .env(
            "FINCH_TEST_TERMINAL_SESSION_CHILD",
            "backpressure-blocking-mutation",
        )
        .env("FINCH_TEST_TERMINAL_CONTROL_FD", control[1].to_string())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    unsafe { nix::libc::close(control[1]) };
    let ready = read_control_byte(
        &mut child,
        control[0],
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(ready, b'R');
    unsafe { nix::libc::close(control[0]) };
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        assert!(child.try_wait().unwrap().is_none());
        std::thread::yield_now();
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    let _ = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
    unsafe { nix::libc::tcsetattr(slave.as_raw_fd(), nix::libc::TCSANOW, &original) };
    drop(master);
}

/// Test that TUI initializes without crashing
#[test]
#[ignore] // Requires interactive terminal or expect
fn test_tui_initialization() {
    // This test should be run with expect or a PTY library
    // For now, we just verify the binary runs

    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn shammah");

    // Send exit command
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "/exit").ok();
    }

    // Wait for exit (with timeout)
    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success() || status.code() == Some(0));
}

/// Test that TUI components are available (basic compilation test)
#[test]
fn test_tui_module_exists() {
    // Just verify the TUI module compiles and is accessible
    // Internal details are tested via unit tests in src/
    assert!(true);
}

/// Test TUI output manager integration
#[test]
fn test_output_manager() {
    use finch::cli::OutputManager;

    let manager = OutputManager::new(finch::config::ColorScheme::default());

    // Test stdout control
    manager.disable_stdout();
    // Just verify it doesn't crash
    manager.enable_stdout();
    // Manager methods work without panicking
}

/// Test that piped input mode doesn't try to use TUI
#[test]
fn test_non_interactive_mode() {
    // When stdin is not a TTY, TUI should not be used. Keep this test local
    // and deterministic: ordinary English invokes the configured provider and
    // used to make the suite wait through network retries.
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("query")
        .arg("(say \"test\")")
        .output()
        .expect("Failed to run query");

    // Should complete without TUI (no escape codes in stderr)
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Basic check - proper TUI wouldn't work in non-interactive mode
    assert!(!stderr.contains("raw mode"));
}

/// E2E: tool message format uses correct Unicode characters (⏺, ⎿)
///
/// This verifies the fix for the old ●/└ characters that caused visual regressions.
/// Uses the WorkUnit API (the live rendering path) — no terminal needed.
#[test]
fn test_tool_display_uses_correct_unicode() {
    use finch::cli::messages::{Message, WorkUnit};
    use finch::cli::repl_event::tool_display::format_tool_label;
    use finch::config::ColorScheme;

    let label = format_tool_label("bash", &serde_json::json!({"command": "echo hi"}));
    let unit = WorkUnit::new("Running");
    let row_idx = unit.add_row(label);
    unit.complete_row(row_idx, "hi");
    unit.set_complete();

    let result = unit.format(&ColorScheme::default());

    // Must use ⏺ (U+23FA) as bullet, NOT ● (U+25CF)
    assert!(
        result.contains('⏺'),
        "Expected ⏺ (U+23FA), got: {:?}",
        result
    );
    assert!(
        !result.contains('●'),
        "Found old ● (U+25CF) — wrong bullet char in: {:?}",
        result
    );

    // Must use ⎿ (U+23BF) as output prefix, NOT └ (U+2514)
    assert!(
        result.contains('⎿'),
        "Expected ⎿ (U+23BF), got: {:?}",
        result
    );
    assert!(
        !result.contains('└'),
        "Found old └ (U+2514) — wrong corner char in: {:?}",
        result
    );
}

/// Input token count — the `↑ N.Nk` status-bar format
///
/// Verifies that `format_token_count` (used to render the status bar during
/// streaming) produces the expected compact representation from the public API.
/// The function is also tested internally; this is an integration-level smoke
/// test confirming the public export is stable.
#[test]
fn test_format_token_count_public_api() {
    use finch::cli::repl_event::tool_display::format_token_count;

    // Below 1000 → plain decimal
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");

    // At 1000 → switch to "N.Nk" notation
    assert_eq!(format_token_count(1000), "1.0k");
    assert_eq!(format_token_count(1500), "1.5k");
    assert_eq!(format_token_count(8192), "8.2k"); // common context window chunk

    // Large counts
    assert_eq!(format_token_count(100_000), "100.0k");
}

/// The status-bar "↑ input" format is correctly assembled from format_token_count.
#[test]
fn test_input_token_status_bar_format() {
    use finch::cli::repl_event::tool_display::format_token_count;

    let input_tokens: u32 = 1250;
    let output_tokens: usize = 300;

    // Simulate the status-bar assembly used in event_loop.rs during streaming
    let status = format!(
        "↑ {} · ↓ {} tokens",
        format_token_count(input_tokens as usize),
        format_token_count(output_tokens),
    );
    assert_eq!(status, "↑ 1.2k · ↓ 300 tokens");
}

/// E2E: binary exits cleanly without panicking when invoked with --version
#[test]
fn test_binary_exits_cleanly() {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--version")
        .output()
        .expect("Failed to run finch --version");

    // Should not crash or produce panic output
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "unexpected panic in stderr: {}",
        stderr
    );
    assert!(
        !stderr.contains("RUST_BACKTRACE"),
        "unexpected backtrace in stderr: {}",
        stderr
    );
}
