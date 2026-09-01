#[path = "../../../src/cli/tui/terminal_lifecycle.rs"]
mod terminal_lifecycle;
#[path = "../../../src/cli/tui/terminal_protocol.rs"]
mod terminal_protocol;

#[cfg(windows)]
use std::io::IsTerminal;
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
    let renderer = terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    renderer.write(b"portable-frame").unwrap();
    assert!(terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .is_err());
    FAIL_CLEANUP.store(true, Ordering::Release);
    assert!(renderer.cleanup().is_err());
    assert!(renderer.write(b"stale-frame").is_err());
    renderer.cleanup().unwrap();

    let renderer = Arc::new(
        terminal_lifecycle::PortableRendererSession::activate(
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
    assert!(terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .is_err());

    terminal_lifecycle::supervised_set_output_gate_pause(false);
    assert!(writer.join().unwrap().is_err());
    renderer.cleanup().unwrap();

    let replacement = terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    assert!(renderer.write(b"old-generation").is_err());
    replacement.cleanup().unwrap();

    // Exercise the production actor boundary, not only its admission gate.
    // A staged output/cleanup timeout remains fail-closed until the actor is
    // resumed, and the staged frame is rejected after CLEANING revocation.
    let renderer = terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    let effects_before = terminal_lifecycle::supervised_actor_write_effects();
    terminal_lifecycle::supervised_set_actor_pause(true);
    let writer_renderer = Arc::new(renderer);
    let writer_clone = Arc::clone(&writer_renderer);
    let writer = std::thread::spawn(move || writer_clone.write(b"staged-frame"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminal_lifecycle::supervised_actor_is_paused() {
        assert!(Instant::now() < deadline, "portable actor did not pause");
        std::thread::yield_now();
    }
    assert!(writer.join().unwrap().is_err());
    terminal_lifecycle::supervised_set_actor_pause(false);
    assert_eq!(writer_renderer.write(b"live-frame").unwrap(), 10);
    assert_eq!(
        terminal_lifecycle::supervised_actor_write_effects(),
        effects_before + 1
    );

    let flush_effects_before = terminal_lifecycle::supervised_actor_flush_effects();
    terminal_lifecycle::supervised_set_actor_pause(true);
    let generation = writer_renderer.generation();
    let flush = std::thread::spawn(move || terminal_lifecycle::flush_generation(generation));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminal_lifecycle::supervised_actor_is_paused() {
        assert!(Instant::now() < deadline, "portable actor did not pause");
        std::thread::yield_now();
    }
    assert!(flush.join().unwrap().is_err());
    terminal_lifecycle::supervised_set_actor_pause(false);
    terminal_lifecycle::flush_generation(writer_renderer.generation()).unwrap();
    assert_eq!(
        terminal_lifecycle::supervised_actor_flush_effects(),
        flush_effects_before + 1
    );
    writer_renderer.cleanup().unwrap();

    FAIL_ACTIVATION.store(true, Ordering::Release);
    FAIL_CLEANUP.store(true, Ordering::Release);
    let activation_error = match terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    ) {
        Ok(_) => panic!("activation and rollback failure was accepted"),
        Err(error) => error,
    };
    let activation_error = activation_error.to_string();
    assert!(activation_error.contains("activation probe"));
    assert!(activation_error.contains("rollback probe"));
    assert!(terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .is_err());
    terminal_lifecycle::cleanup_active(cleanup_protocols).unwrap();

    #[cfg(windows)]
    if std::io::stdout().is_terminal() {
        // When CI supplies a real console/ConPTY, construct and exercise the
        // actual cfg(not(unix)) raw/protocol callbacks behind this same actor.
        let actual = terminal_lifecycle::PortableRendererSession::activate(
            terminal_protocol::activate,
            terminal_protocol::cleanup,
        )
        .expect("activate actual Windows renderer lifecycle");
        actual.write(b"finch-portable-actor").unwrap();
        actual
            .cleanup()
            .expect("cleanup actual Windows renderer lifecycle");
    } else {
        // Hosted Windows runners normally redirect stdout and provide no
        // ConPTY. Keep the acceptance gap explicit instead of treating helper
        // bytes as proof of real console/raw-mode behavior.
        eprintln!("FINCH_WINDOWS_CONPTY_ACCEPTANCE_GATED:no-console");
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
