//! Mouse tracking for the in-app conversation scroll view.
//!
//! `EnableMouseCapture` is what makes wheel-and-click hit testing possible at
//! all: with tracking off the terminal never reports a wheel or a click, so
//! the only scroll surface is the host's own native history. #806 decides the
//! #443 fork toward in-app conversation scroll (like Claude Code and Grok,
//! not Codex's native-history reader), so tracking is **held by default**:
//! wheels hit the conversation ScrollView, nested tool-result viewports, or
//! nothing, and clicks hit the disclosure and composer hitboxes.
//!
//! The #441 release-on-first-wheel policy is retired: the wheel is no longer
//! handed back to the terminal, because native scrollback is not the reader —
//! `canonical_commit` keeps the fully expanded copyable record there instead.
//! Native text drag-selection under held capture was the #221 trade;
//! `selection.rs` is Finch's own in-app replacement (click-drag highlight +
//! clipboard copy) rather than giving capture back to the terminal, so #221
//! no longer needs the terminal's own selection. A future opt-out preference
//! is #244.
//!
//! Shutdown, panic, suspend, and emergency restore always emit
//! `DisableMouseCapture` so a session that held tracking cannot leak it into
//! the shell.

use std::io::{self, Write};

use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::style::{Print, ResetColor};
use crossterm::terminal::LeaveAlternateScreen;

/// Whether Finch currently holds mouse tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MouseTracking {
    /// The terminal reports mouse events to Finch: wheels and clicks hit the
    /// in-app widget hitboxes (conversation scroll view, tool viewports,
    /// disclosure). This is the production default since #806.
    Held,
    /// Reserved for the future opt-out preference (#244): the host terminal
    /// owns the wheel and click-drag selection, and accordion disclosure
    /// stays on the keyboard.
    #[allow(dead_code)]
    Off,
}

impl MouseTracking {
    /// Production default: capture held, so the conversation ScrollView owns
    /// the wheel (#806).
    pub(super) const DEFAULT: Self = Self::Held;
}

/// Crossterm `EnableMouseCapture` bytes, used to assert their presence.
#[cfg(test)]
pub(super) fn enable_mouse_capture_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    execute!(&mut out, EnableMouseCapture).expect("encode EnableMouseCapture");
    out
}

/// Crossterm `DisableMouseCapture` bytes, used to assert restore symmetry.
#[cfg(test)]
pub(super) fn disable_mouse_capture_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    execute!(&mut out, DisableMouseCapture).expect("encode DisableMouseCapture");
    out
}

#[cfg(test)]
pub(super) fn contains_enable_mouse_capture(bytes: &[u8]) -> bool {
    let needle = enable_mouse_capture_bytes();
    if needle.is_empty() {
        return false;
    }
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
pub(super) fn contains_disable_mouse_capture(bytes: &[u8]) -> bool {
    let needle = disable_mouse_capture_bytes();
    if needle.is_empty() {
        return false;
    }
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Post-`enable_raw_mode` sequences written by [`super::TuiRenderer::new`].
///
/// Mouse capture is enabled so wheels and clicks reach the in-app widget
/// hitboxes (#806); the wheel scrolls the conversation, never native history.
pub(super) fn write_startup_terminal_modes(out: &mut impl Write) -> io::Result<()> {
    // Bracketed paste cannot corrupt the terminal on unclean exit.
    let _ = execute!(out, EnableBracketedPaste, EnableMouseCapture);
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    );
    execute!(out, cursor::Show)
}

fn write_enable_if_held(out: &mut impl Write, tracking: MouseTracking) {
    if tracking == MouseTracking::Held {
        let _ = execute!(out, EnableMouseCapture);
    }
}

/// Resume after [`super::TuiRenderer::suspend`]. A held session re-enables
/// capture; the reserved opt-out (#244) writes nothing.
pub(super) fn write_resume_terminal_modes(
    out: &mut impl Write,
    tracking: MouseTracking,
) -> io::Result<()> {
    write_enable_if_held(out, tracking);
    Ok(())
}

/// Reacquire modes after [`super::emergency_restore_terminal`].
pub(super) fn write_resume_after_emergency_modes(
    out: &mut impl Write,
    tracking: MouseTracking,
) -> io::Result<()> {
    write_enable_if_held(out, tracking);
    let _ = execute!(
        out,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        cursor::Show,
    );
    Ok(())
}

/// Shutdown / explicit restore. Always disable mouse capture so a session that
/// held it cannot leak reporting into the shell.
pub(super) fn write_shutdown_terminal_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(
        out,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste,
        cursor::Show,
        ResetColor,
    )
}

pub(super) fn write_suspend_terminal_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(out, DisableMouseCapture)
}

pub(super) fn write_emergency_restore_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(
        out,
        LeaveAlternateScreen,
        DisableMouseCapture,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        cursor::Show,
        ResetColor,
        Print("\r\n"),
    )
}

pub(super) fn write_panic_restore_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(out, DisableMouseCapture, PopKeyboardEnhancementFlags,)
}

pub(super) fn is_wheel(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseButton;

    fn tracking_on() -> Vec<u8> {
        enable_mouse_capture_bytes()
    }

    #[test]
    fn test_default_tracking_holds_capture_for_the_conversation_scrollview() {
        assert_eq!(
            MouseTracking::DEFAULT,
            MouseTracking::Held,
            "INVARIANT (#806): the production default holds mouse tracking so \
             wheels hit the conversation ScrollView and nested tool viewports \
             instead of falling through to native scrollback"
        );
    }

    #[test]
    fn test_wheel_kinds_are_recognised_for_scroll_dispatch() {
        for kind in [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert!(
                is_wheel(kind),
                "INVARIANT: {kind:?} is a wheel event and must be routed to the \
                 scroll hitboxes (#806)"
            );
        }
        assert!(
            !is_wheel(MouseEventKind::Down(MouseButton::Left)),
            "INVARIANT: a left click is not a wheel; it follows the click path \
             (disclosure toggle, tool-result expansion)"
        );
    }

    fn assert_enables_mouse_capture(bytes: &[u8], path: &str) {
        assert!(
            contains_enable_mouse_capture(bytes),
            "INVARIANT: the default TUI {path} path must hold mouse capture, so \
             wheels scroll the conversation ScrollView (#806). EnableMouseCapture \
             bytes are {:02x?}; terminal received {bytes:02x?}",
            enable_mouse_capture_bytes()
        );
    }

    fn assert_disables_mouse_capture(bytes: &[u8], path: &str) {
        assert!(
            contains_disable_mouse_capture(bytes),
            "INVARIANT: the TUI {path} path must emit DisableMouseCapture so a session \
             that held tracking cannot leak mouse reporting into the shell (#221). \
             DisableMouseCapture bytes are {:02x?}; terminal received {bytes:02x?}",
            disable_mouse_capture_bytes()
        );
    }

    #[test]
    fn test_tui_startup_modes_hold_mouse_capture_for_the_scroll_view() {
        let mut bytes = Vec::new();
        write_startup_terminal_modes(&mut bytes).expect("encode startup modes");
        assert_enables_mouse_capture(&bytes, "startup/enter-raw-mode");
        // Capture is harmless on unclean exit only if every restore path
        // disables it; the restore tests below pin those paths.
    }

    #[test]
    fn test_tui_resume_modes_hold_mouse_capture_by_default() {
        let mut bytes = Vec::new();
        write_resume_terminal_modes(&mut bytes, MouseTracking::DEFAULT)
            .expect("encode resume modes");
        assert_enables_mouse_capture(&bytes, "resume");
    }

    #[test]
    fn test_tui_resume_after_emergency_modes_hold_mouse_capture_by_default() {
        let mut bytes = Vec::new();
        write_resume_after_emergency_modes(&mut bytes, MouseTracking::DEFAULT)
            .expect("encode resume-after-emergency modes");
        assert_enables_mouse_capture(&bytes, "resume after emergency restore");
    }

    #[test]
    fn test_resume_reenables_mouse_capture_only_when_held() {
        let mut held = Vec::new();
        write_resume_terminal_modes(&mut held, MouseTracking::Held).expect("encode held resume");
        assert_eq!(
            held,
            tracking_on(),
            "INVARIANT: a Held session must restore EnableMouseCapture on resume \
             (#806). terminal received {held:02x?}"
        );

        let mut off = Vec::new();
        write_resume_terminal_modes(&mut off, MouseTracking::Off).expect("encode off resume");
        assert!(
            off.is_empty(),
            "INVARIANT: an opted-out resume must write nothing, not EnableMouseCapture \
             (#244). terminal received {off:02x?}"
        );
    }

    #[test]
    fn test_tui_restore_paths_disable_mouse_capture() {
        let mut shutdown = Vec::new();
        write_shutdown_terminal_modes(&mut shutdown).expect("encode shutdown");
        assert_disables_mouse_capture(&shutdown, "shutdown");

        let mut suspend = Vec::new();
        write_suspend_terminal_modes(&mut suspend).expect("encode suspend");
        assert_disables_mouse_capture(&suspend, "suspend");

        let mut emergency = Vec::new();
        write_emergency_restore_modes(&mut emergency).expect("encode emergency restore");
        assert_disables_mouse_capture(&emergency, "emergency restore");

        let mut panic_restore = Vec::new();
        write_panic_restore_modes(&mut panic_restore).expect("encode panic");
        assert_disables_mouse_capture(&panic_restore, "panic");
    }
}
