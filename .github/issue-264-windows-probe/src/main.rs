#[path = "../../../src/cli/tui/terminal_lifecycle.rs"]
mod terminal_lifecycle;
#[path = "../../../src/cli/tui/terminal_protocol.rs"]
mod terminal_protocol;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static FAIL_CLEANUP: AtomicBool = AtomicBool::new(false);
static FAIL_ACTIVATION: AtomicBool = AtomicBool::new(false);

fn activate_protocols() -> std::io::Result<()> {
    if FAIL_ACTIVATION.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other("activation probe"));
    }
    Ok(())
}

fn cleanup_protocols() -> std::io::Result<()> {
    if FAIL_CLEANUP.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other("rollback probe"));
    }
    Ok(())
}

fn run_probe() {
    let mut activation = Vec::new();
    terminal_protocol::write_activation(&mut activation).unwrap();
    let mut cleanup = Vec::new();
    terminal_protocol::write_reset(&mut cleanup).unwrap();

    #[cfg(not(unix))]
    {
        let _activate: fn() -> std::io::Result<()> = terminal_protocol::activate;
        let _cleanup: fn() -> std::io::Result<()> = terminal_protocol::cleanup;
    }

    // This is the exact lifecycle/output actor owned by cfg(not(unix))
    // TuiRenderer, rather than a copy of its protocol helpers.
    let renderer = terminal_lifecycle::PortableRendererSession::activate_supervised(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    renderer.write(b"portable-frame").unwrap();
    assert!(
        terminal_lifecycle::PortableRendererSession::activate_supervised(
            activate_protocols,
            cleanup_protocols,
        )
        .is_err()
    );
    FAIL_CLEANUP.store(true, Ordering::Release);
    assert!(renderer.cleanup().is_err());
    assert!(renderer.write(b"stale-frame").is_err());
    renderer.cleanup().unwrap();

    let renderer = Arc::new(
        terminal_lifecycle::PortableRendererSession::activate_supervised(
            activate_protocols,
            cleanup_protocols,
        )
        .unwrap(),
    );
    terminal_lifecycle::supervised_set_output_gate_pause(true);
    let writer_renderer = Arc::clone(&renderer);
    let writer = std::thread::spawn(move || writer_renderer.write(b"late-frame"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminal_lifecycle::supervised_output_gate_is_paused() {
        assert!(Instant::now() < deadline, "portable writer did not pause");
        std::thread::yield_now();
    }

    let cleanup_started = Instant::now();
    assert!(renderer.cleanup().is_err());
    assert!(cleanup_started.elapsed() < Duration::from_millis(250));
    assert!(
        terminal_lifecycle::PortableRendererSession::activate_supervised(
            activate_protocols,
            cleanup_protocols,
        )
        .is_err()
    );

    terminal_lifecycle::supervised_set_output_gate_pause(false);
    assert!(writer.join().unwrap().is_err());
    renderer.cleanup().unwrap();

    let replacement = terminal_lifecycle::PortableRendererSession::activate_supervised(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    assert!(renderer.write(b"old-generation").is_err());
    replacement.cleanup().unwrap();

    // Exercise the production actor's claimed-but-not-effecting edge. Before
    // this edge existed, wait_effect_reply_until changed into an unbounded
    // recv and a stalled Write/Flush parked both caller and cleanup forever.
    let renderer = terminal_lifecycle::PortableRendererSession::activate_supervised(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    let effects_before = terminal_lifecycle::supervised_actor_write_effects();
    terminal_lifecycle::supervised_set_actor_effect_pause(true);
    let writer_renderer = Arc::new(renderer);
    let writer_clone = Arc::clone(&writer_renderer);
    let writer = std::thread::spawn(move || writer_clone.write(b"staged-frame"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminal_lifecycle::supervised_actor_effect_is_paused() {
        assert!(Instant::now() < deadline, "portable write was not claimed");
        std::thread::yield_now();
    }
    let started = Instant::now();
    assert!(writer.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_millis(250));
    let cleanup_started = Instant::now();
    assert!(writer_renderer.cleanup().is_err());
    assert!(cleanup_started.elapsed() < Duration::from_millis(250));
    terminal_lifecycle::supervised_set_actor_effect_pause(false);
    writer_renderer.cleanup().unwrap();
    assert_eq!(
        terminal_lifecycle::supervised_actor_write_effects(),
        effects_before
    );

    let writer_renderer = Arc::new(
        terminal_lifecycle::PortableRendererSession::activate_supervised(
            activate_protocols,
            cleanup_protocols,
        )
        .unwrap(),
    );
    let flush_effects_before = terminal_lifecycle::supervised_actor_flush_effects();
    terminal_lifecycle::supervised_set_actor_effect_pause(true);
    let generation = writer_renderer.generation();
    let flush = std::thread::spawn(move || terminal_lifecycle::flush_generation(generation));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminal_lifecycle::supervised_actor_effect_is_paused() {
        assert!(Instant::now() < deadline, "portable flush was not claimed");
        std::thread::yield_now();
    }
    let started = Instant::now();
    assert!(flush.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_millis(250));
    let cleanup_started = Instant::now();
    assert!(writer_renderer.cleanup().is_err());
    assert!(cleanup_started.elapsed() < Duration::from_millis(250));
    terminal_lifecycle::supervised_set_actor_effect_pause(false);
    writer_renderer.cleanup().unwrap();
    assert_eq!(
        terminal_lifecycle::supervised_actor_flush_effects(),
        flush_effects_before
    );

    FAIL_ACTIVATION.store(true, Ordering::Release);
    FAIL_CLEANUP.store(true, Ordering::Release);
    let activation_error = match terminal_lifecycle::PortableRendererSession::activate_supervised(
        activate_protocols,
        cleanup_protocols,
    ) {
        Ok(_) => panic!("activation and rollback failure was accepted"),
        Err(error) => error,
    };
    let activation_error = activation_error.to_string();
    assert!(activation_error.contains("activation probe"));
    assert!(activation_error.contains("rollback probe"));
    assert!(
        terminal_lifecycle::PortableRendererSession::activate_supervised(
            activate_protocols,
            cleanup_protocols,
        )
        .is_err()
    );
    terminal_lifecycle::cleanup_active(cleanup_protocols).unwrap();

    #[cfg(windows)]
    {
        // WriteConsole/Flush on a console handle has no cancellable or
        // nonblocking contract. The actual cfg(not(unix)) renderer therefore
        // rejects stdout before raw/protocol activation instead of claiming a
        // bounded TUI. ConPTY conformance remains an explicit acceptance gate.
        let error = match terminal_lifecycle::PortableRendererSession::activate(
            terminal_protocol::activate,
            terminal_protocol::cleanup,
        ) {
            Ok(_) => panic!("unsupported Windows stdout activated a renderer"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(terminal_lifecycle::active_generation(), 0);
        eprintln!("FINCH_WINDOWS_CONPTY_ACCEPTANCE_GATED:stdout-not-cancellable");
    }
}

fn main() {
    run_probe();
}

#[cfg(test)]
#[test]
fn test_exact_portable_renderer_actor_production_boundary() {
    let _serial = terminal_lifecycle::supervised_test_lock();
    run_probe();
}
