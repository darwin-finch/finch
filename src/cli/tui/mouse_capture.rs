//! Mouse tracking vs native terminal scrollback.
//!
//! `EnableMouseCapture` is what makes accordion click-to-toggle (and
//! click-to-navigate in the input box) possible. It also makes the terminal
//! deliver wheel ticks to Finch instead of scrolling its own buffer. Issue
//! #441, "Regression: enabling mouse capture broke the crossterm renderer's
//! scrollback path", is that remaining coupling: committed text is already in
//! native history; the user cannot reach it because the wheel is captured.
//!
//! The policy is option A from that issue: keep capture for clicks, release
//! it on a wheel so subsequent ticks belong to the terminal, restore it on the
//! next keypress. The first wheel tick is consumed as the release.

use std::io::Write;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, MouseEventKind};
use crossterm::execute;

/// Whether Finch currently holds mouse tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MouseTracking {
    /// Terminal reports mouse events to Finch. Clicks work; the wheel does not
    /// scroll native history.
    Held,
    /// Terminal owns the wheel, so native scrollback is reachable. Restored on
    /// the next keypress.
    ReleasedForNativeScroll,
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
}
