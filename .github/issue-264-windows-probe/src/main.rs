#[path = "../../../src/cli/tui/terminal_lifecycle.rs"]
mod terminal_lifecycle;
#[path = "../../../src/cli/tui/terminal_protocol.rs"]
mod terminal_protocol;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static FAIL_CLEANUP: AtomicBool = AtomicBool::new(false);

fn activate_protocols() -> std::io::Result<()> {
    Ok(())
}

fn cleanup_protocols() -> std::io::Result<()> {
    if FAIL_CLEANUP.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other("rollback probe"));
    }
    Ok(())
}

fn main() {
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
    let mut renderer_output = Vec::new();
    renderer
        .write(&mut renderer_output, b"portable-frame")
        .unwrap();
    assert!(terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .is_err());
    FAIL_CLEANUP.store(true, Ordering::Release);
    assert!(renderer.cleanup().is_err());
    assert!(renderer
        .write(&mut renderer_output, b"stale-frame")
        .is_err());
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
    let writer = std::thread::spawn(move || {
        let mut writer_output = Vec::new();
        let result = writer_renderer.write(&mut writer_output, b"late-frame");
        (result, writer_output)
    });
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
    let (writer_result, writer_output) = writer.join().unwrap();
    assert!(writer_result.is_err());
    assert!(writer_output.is_empty());
    renderer.cleanup().unwrap();

    let replacement = terminal_lifecycle::PortableRendererSession::activate(
        activate_protocols,
        cleanup_protocols,
    )
    .unwrap();
    assert!(renderer
        .write(&mut renderer_output, b"old-generation")
        .is_err());
    replacement.cleanup().unwrap();
}
