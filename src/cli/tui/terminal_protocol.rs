#[cfg(not(unix))]
use crossterm::terminal;
use crossterm::{cursor, execute, style};
use std::io::{self, Write};

/// Emit every terminal protocol mode Finch acquires around its inline TUI.
pub(crate) fn write_activation(writer: &mut impl Write) -> io::Result<()> {
    execute!(
        writer,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        ),
        cursor::Show,
    )
}

/// Emit the exact reverse protocol sequence plus a visible, reset cursor row.
pub(crate) fn write_reset(writer: &mut impl Write) -> io::Result<()> {
    execute!(
        writer,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        cursor::Show,
        style::ResetColor,
        style::Print("\r\n"),
    )
}

/// Acquire raw mode and every protocol mode transactionally on non-Unix hosts.
#[cfg(not(unix))]
pub(crate) fn activate() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = write_activation(&mut stdout) {
        let _ = write_reset(&mut stdout);
        let _ = terminal::disable_raw_mode();
        return Err(error);
    }
    Ok(())
}

/// Restore every portable protocol mode and raw mode on non-Unix hosts.
#[cfg(not(unix))]
pub(crate) fn cleanup() -> io::Result<()> {
    let mut stdout = io::stdout();
    let reset_result = write_reset(&mut stdout);
    let raw_result = terminal::disable_raw_mode();
    reset_result.and(raw_result)
}
