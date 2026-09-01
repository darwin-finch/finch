#[path = "../../../src/cli/tui/terminal_lifecycle.rs"]
mod terminal_lifecycle;
#[path = "../../../src/cli/tui/terminal_protocol.rs"]
mod terminal_protocol;

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

    let lease = terminal_lifecycle::ExclusiveTerminalLease::activate(|| Ok(())).unwrap();
    assert!(terminal_lifecycle::ExclusiveTerminalLease::activate(|| Ok(())).is_err());
    lease.cleanup(|| Ok(())).unwrap();
}
