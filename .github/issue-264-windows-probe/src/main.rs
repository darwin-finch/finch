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
        let _cleanup: fn() = terminal_protocol::cleanup;
    }
}
