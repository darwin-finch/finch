//! Mouse tracking vs native terminal scrollback.
//!
//! `EnableMouseCapture` is what makes accordion click-to-toggle (and
//! click-to-navigate in the input box) possible. It also makes the terminal
//! deliver every drag to Finch instead of the host's own selection, and every
//! wheel tick instead of native scrollback. Issue #221 requires native
//! click-drag copy by default; accordion expand/collapse stays on the
//! keyboard. Capture is therefore off unless an opt-in path later holds it.
//!
//! When tracking *is* held, issue #441's option A still applies: release on a
//! wheel so subsequent ticks belong to the terminal, restore on the next
//! keypress. The first wheel tick is consumed as the release. Shutdown, panic,
//! suspend, and emergency restore always emit `DisableMouseCapture` so a
//! session that did hold tracking cannot leak it into the shell.

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
    /// Default: the host terminal owns click-drag selection and the wheel
    /// (#221). Accordion disclosure stays on the keyboard.
    Off,
    /// Terminal reports mouse events to Finch. Clicks work; the wheel does not
    /// scroll native history. Reached only if an opt-in path enables capture.
    Held,
    /// Terminal owns the wheel, so native scrollback is reachable. Restored on
    /// the next keypress, and only if tracking was previously [`Self::Held`].
    ReleasedForNativeScroll,
}

impl MouseTracking {
    /// Production default: native selection, no mouse reporting.
    pub(super) const DEFAULT: Self = Self::Off;
}

/// Crossterm `EnableMouseCapture` bytes, used to assert their absence.
pub(super) fn enable_mouse_capture_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    execute!(&mut out, EnableMouseCapture).expect("encode EnableMouseCapture");
    out
}

/// Crossterm `DisableMouseCapture` bytes, used to assert restore symmetry.
pub(super) fn disable_mouse_capture_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    execute!(&mut out, DisableMouseCapture).expect("encode DisableMouseCapture");
    out
}

pub(super) fn contains_enable_mouse_capture(bytes: &[u8]) -> bool {
    let needle = enable_mouse_capture_bytes();
    if needle.is_empty() {
        return false;
    }
    bytes.windows(needle.len()).any(|window| window == needle)
}

pub(super) fn contains_disable_mouse_capture(bytes: &[u8]) -> bool {
    let needle = disable_mouse_capture_bytes();
    if needle.is_empty() {
        return false;
    }
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Post-`enable_raw_mode` sequences written by [`super::TuiRenderer::new`].
///
/// Mouse capture must not appear here: click-drag belongs to the host terminal
/// (#221).
pub(super) fn write_startup_terminal_modes(out: &mut impl Write) -> io::Result<()> {
    // Bracketed paste cannot corrupt the terminal on unclean exit.
    // EnableMouseCapture is omitted so click-drag belongs to the host (#221).
    let _ = execute!(out, EnableBracketedPaste);
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

/// Resume after [`super::TuiRenderer::suspend`]. Default tracking does not
/// re-enable mouse capture; only an opt-in [`MouseTracking::Held`] session does.
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

/// Release tracking so later wheel ticks scroll native history.
///
/// Idempotent: a second wheel while already released writes nothing.
pub(super) fn release_for_native_scroll(
    out: &mut impl Write,
    tracking: MouseTracking,
) -> MouseTracking {
    if tracking != MouseTracking::Held {
        return tracking;
    }
    let _ = execute!(out, DisableMouseCapture);
    MouseTracking::ReleasedForNativeScroll
}

/// Restore tracking after the user is interacting with the live area again.
///
/// Idempotent: a keypress while already held writes nothing.
pub(super) fn restore_after_interaction(
    out: &mut impl Write,
    tracking: MouseTracking,
) -> MouseTracking {
    if tracking != MouseTracking::ReleasedForNativeScroll {
        return tracking;
    }
    let _ = execute!(out, EnableMouseCapture);
    MouseTracking::Held
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseButton;

    fn tracking_on() -> Vec<u8> {
        let mut out = Vec::new();
        execute!(&mut out, EnableMouseCapture).expect("encode EnableMouseCapture");
        out
    }

    fn tracking_off() -> Vec<u8> {
        let mut out = Vec::new();
        execute!(&mut out, DisableMouseCapture).expect("encode DisableMouseCapture");
        out
    }

    #[test]
    fn test_wheel_releases_mouse_tracking_so_native_scrollback_is_reachable() {
        for kind in [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            let mut bytes = Vec::new();
            let next = if is_wheel(kind) {
                release_for_native_scroll(&mut bytes, MouseTracking::Held)
            } else {
                MouseTracking::Held
            };
            assert_eq!(
                next,
                MouseTracking::ReleasedForNativeScroll,
                "INVARIANT: a wheel tick releases mouse tracking so the terminal \
                 owns subsequent wheel events and native scrollback is reachable \
                 (#441). kind={kind:?}"
            );
            assert_eq!(
                bytes,
                tracking_off(),
                "INVARIANT: the release is DisableMouseCapture, the sequence the \
                 terminal needs to stop reporting the wheel (#441). kind={kind:?} \
                 terminal received {bytes:?}"
            );
        }
    }

    #[test]
    fn test_second_wheel_does_not_repeat_the_scrollback_release() {
        let mut bytes = Vec::new();
        let tracking = release_for_native_scroll(&mut bytes, MouseTracking::Held);
        bytes.clear();
        let tracking = release_for_native_scroll(&mut bytes, tracking);
        assert_eq!(tracking, MouseTracking::ReleasedForNativeScroll);
        assert!(
            bytes.is_empty(),
            "INVARIANT: a second wheel while tracking is already released must \
             not emit another DisableMouseCapture (#441). terminal received \
             {bytes:?}"
        );
    }

    #[test]
    fn test_left_click_does_not_release_mouse_tracking_for_native_scrollback() {
        let mut bytes = Vec::new();
        assert!(
            !is_wheel(MouseEventKind::Down(MouseButton::Left)),
            "INVARIANT: a left click is not a wheel; accordion click-to-toggle \
             must keep mouse tracking (#441)"
        );
        let next = if is_wheel(MouseEventKind::Down(MouseButton::Left)) {
            release_for_native_scroll(&mut bytes, MouseTracking::Held)
        } else {
            MouseTracking::Held
        };
        assert_eq!(next, MouseTracking::Held);
        assert!(
            bytes.is_empty(),
            "INVARIANT: clicks must not disable mouse tracking; that would drop \
             click-to-toggle for the rest of the gesture (#441). terminal \
             received {bytes:?}"
        );
    }

    #[test]
    fn test_keypress_restores_mouse_tracking_after_native_scrollback() {
        let mut bytes = Vec::new();
        let tracking =
            restore_after_interaction(&mut bytes, MouseTracking::ReleasedForNativeScroll);
        assert_eq!(
            tracking,
            MouseTracking::Held,
            "INVARIANT: the next keypress after a wheel restores mouse tracking \
             so clicks work again (#441)"
        );
        assert_eq!(
            bytes,
            tracking_on(),
            "INVARIANT: the restore is EnableMouseCapture (#441). terminal \
             received {bytes:?}"
        );
    }

    #[test]
    fn test_keypress_while_tracking_held_does_not_reenable_for_scrollback() {
        let mut bytes = Vec::new();
        let tracking = restore_after_interaction(&mut bytes, MouseTracking::Held);
        assert_eq!(tracking, MouseTracking::Held);
        assert!(
            bytes.is_empty(),
            "INVARIANT: a keypress while tracking is already held must not emit \
             another EnableMouseCapture (#441). terminal received {bytes:?}"
        );
    }

    fn assert_no_enable_mouse_capture(bytes: &[u8], path: &str) {
        assert!(
            !contains_enable_mouse_capture(bytes),
            "INVARIANT: the default TUI {path} path must not emit EnableMouseCapture, \
             so click-drag selection stays with the host terminal (#221). \
             EnableMouseCapture bytes are {:02x?}; terminal received {bytes:02x?}",
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
    fn test_tui_startup_modes_do_not_enable_mouse_capture() {
        let mut bytes = Vec::new();
        write_startup_terminal_modes(&mut bytes).expect("encode startup modes");
        assert_no_enable_mouse_capture(&bytes, "startup/enter-raw-mode");
    }

    #[test]
    fn test_tui_resume_modes_do_not_enable_mouse_capture_by_default() {
        let mut bytes = Vec::new();
        write_resume_terminal_modes(&mut bytes, MouseTracking::DEFAULT)
            .expect("encode resume modes");
        assert_no_enable_mouse_capture(&bytes, "resume");
    }

    #[test]
    fn test_tui_resume_after_emergency_modes_do_not_enable_mouse_capture_by_default() {
        let mut bytes = Vec::new();
        write_resume_after_emergency_modes(&mut bytes, MouseTracking::DEFAULT)
            .expect("encode resume-after-emergency modes");
        assert_no_enable_mouse_capture(&bytes, "resume after emergency restore");
    }

    #[test]
    fn test_resume_reenables_mouse_capture_only_when_held() {
        let mut held = Vec::new();
        write_resume_terminal_modes(&mut held, MouseTracking::Held).expect("encode held resume");
        assert_eq!(
            held,
            enable_mouse_capture_bytes(),
            "INVARIANT: an opt-in Held session must restore EnableMouseCapture on \
             resume (#221 / #244). terminal received {held:02x?}"
        );

        let mut off = Vec::new();
        write_resume_terminal_modes(&mut off, MouseTracking::Off).expect("encode off resume");
        assert!(
            off.is_empty(),
            "INVARIANT: default-off resume must write nothing, not EnableMouseCapture \
             (#221). terminal received {off:02x?}"
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
        write_panic_restore_modes(&mut panic_restore).expect("encode panic restore");
        assert_disables_mouse_capture(&panic_restore, "panic");
    }

    #[test]
    fn test_default_tracking_does_not_restore_capture_on_keypress() {
        let mut bytes = Vec::new();
        let next = restore_after_interaction(&mut bytes, MouseTracking::DEFAULT);
        assert_eq!(
            next,
            MouseTracking::DEFAULT,
            "INVARIANT: a keypress with default-off tracking must not switch into \
             Held and re-enable capture (#221). tracking was {next:?}"
        );
        assert_no_enable_mouse_capture(&bytes, "keypress restore with default tracking");
    }
}
