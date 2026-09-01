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

    let preserves_host_signal = matches!(mode.as_str(), "binary-owner-drop" | "owner-windows");
    if preserves_host_signal {
        let mut action = unsafe { std::mem::zeroed::<nix::libc::sigaction>() };
        action.sa_sigaction = embedding_signal_handler as *const () as usize;
        unsafe { nix::libc::sigemptyset(&mut action.sa_mask) };
        anyhow::ensure!(
            unsafe { nix::libc::sigaction(nix::libc::SIGTERM, &action, std::ptr::null_mut()) } == 0,
            "install host handler before binary ownership"
        );
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
            let (signal_read, signal_write) = signals
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("binary signal owner missing"))?
                .supervised_signal_fds()?;
            for signal_fd in [signal_read, signal_write] {
                let descriptor = unsafe { nix::libc::fcntl(signal_fd, nix::libc::F_GETFD) };
                let status = unsafe { nix::libc::fcntl(signal_fd, nix::libc::F_GETFL) };
                anyhow::ensure!(descriptor & nix::libc::FD_CLOEXEC != 0);
                anyhow::ensure!(status & nix::libc::O_NONBLOCK != 0);
            }
            let mut exec_child = Command::new(std::env::current_exe()?)
                .args(["--exact", "tui_terminal_session_child"])
                .env("FINCH_TEST_TERMINAL_SESSION_CHILD", "fd-check")
                .env(
                    "FINCH_TEST_RESTORE_FDS",
                    format!("{fd},{signal_read},{signal_write}"),
                )
                .spawn()?;
            let status = wait_for_child(&mut exec_child, Instant::now() + Duration::from_secs(2));
            anyhow::ensure!(status.success(), "restore fd inherited by exec child");
            renderer.shutdown()
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
            signals.supervised_fill_signal_transport()?;
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
            signals.supervised_fill_signal_transport()?;
            marker(b"FINCH_SIGNAL_QUEUE_FULL")?;
            let mut acknowledge = 0_u8;
            std::io::stdin().read_exact(std::slice::from_mut(&mut acknowledge))?;
            anyhow::ensure!(acknowledge == b'G', "invalid full-queue acknowledgement");
            signals.supervised_resume_signal_listener()?;
            loop {
                std::thread::park();
            }
        }
        "handler-reuse" => {
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

            let second_signals = finch::cli::tui::BinaryTerminalSession::install()?
                .ok_or_else(|| anyhow::anyhow!("replacement binary signal owner missing"))?;
            let mut second = new_renderer()?;
            finch::cli::tui::supervised_set_terminal_signal_handler_pause(false)?;
            handler_thread
                .join()
                .map_err(|_| anyhow::anyhow!("paused signal handler thread panicked"))?;
            anyhow::ensure!(
                observed_errno.load(Ordering::Acquire) == nix::libc::EDOM,
                "terminal signal handler changed the interrupted thread's errno"
            );
            for _ in 0..10_000 {
                std::thread::yield_now();
            }
            second.shutdown()?;
            drop(second_signals);
            marker(b"FINCH_HANDLER_REUSE_SAFE")?;
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
            finch::cli::global_output::set_global_tui_renderer(renderer);
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
fn test_binary_terminal_signal_is_not_lost_when_transport_is_full() {
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
        b"FINCH_SIGNAL_QUEUE_FULL",
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
    let (mut master, slave) = open_owned_pty();
    let original = terminal_modes(&slave);
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-terminal-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_MAIN_PANIC_AFTER_ACTIVE", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn Finch main panic-hook probe");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(20));
    let mut transcript = Vec::new();
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
    assert!(String::from_utf8_lossy(&transcript)
        .contains("supervised panic after Finch terminal activation"));
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
        "signal-create-failure",
        "sequential",
        "repeated",
        "overlapping-cleanup",
    ] {
        let (mut child, _master, slave, original) = spawn_terminal_child(mode);
        assert!(wait_for_child(&mut child, Instant::now() + Duration::from_secs(5)).success());
        assert_eq!(terminal_modes(&slave), original, "{mode} termios mismatch");
    }
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
fn test_signal_transport_drop_is_bounded_and_fd_reuse_is_stale_safe() {
    let _serial = terminal_pty_test_lock();
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for mode in ["signal-stop-full", "handler-reuse"] {
        let (mut child, mut master, slave, original) = spawn_terminal_child(mode);
        let marker = if mode == "signal-stop-full" {
            b"FINCH_SIGNAL_STOP_BOUNDED".as_slice()
        } else {
            b"FINCH_HANDLER_REUSE_SAFE".as_slice()
        };
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
