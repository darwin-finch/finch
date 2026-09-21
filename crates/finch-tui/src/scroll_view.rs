//! The conversation ScrollView: renderer-owned scroll state for the transcript.
//!
//! The live transcript region — the [`super::widgets`] root column's `Flex`
//! `TRANSCRIPT` claim, the leftover frame under the bottom chrome — is a
//! scroll view over the conversation (#806). The state is presentation-only,
//! like the accordion's open set and the tool viewports' offsets: one offset
//! counted in physical terminal rows from the bottom of the retained
//! transcript, where `0` is follow mode (the newest content stays in view).
//!
//! The window is derived, never stored: every paint projects the retained
//! transcript from the output port, splits off the newest `offset` rows,
//! and paints the bottom window of what remains. Resize, reflow, and
//! disclosure changes therefore never desynchronize the view from the content.

use super::accordion::RenderedTranscriptLine;
use super::shadow_buffer;
use super::widgets::Rect;

/// Scroll state for the conversation transcript.
///
/// `offset_from_bottom` is how many physical rows of the newest retained
/// transcript are hidden below the viewport. Zero is follow mode; painting
/// never scrolls on the renderer's behalf — only input moves the offset.
#[derive(Debug, Default)]
pub(crate) struct TranscriptScrollView {
    offset_from_bottom: usize,
    /// The transcript region claimed by the last painted frame, in terminal
    /// rows — the 805 `TRANSCRIPT` Flex claim (the leftover frame under the
    /// bottom chrome). Wheels inside it scroll the conversation.
    claim: Rect,
}

impl TranscriptScrollView {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether a wheel tick at `(column, row)` lands on the conversation
    /// scroll view. Rows below the claim belong to the bottom chrome and are
    /// owned by nobody.
    pub(crate) fn owns(&self, column: u16, row: u16) -> bool {
        let claim = self.claim;
        !claim.is_empty()
            && row >= claim.y as u16
            && row < claim.bottom() as u16
            && column >= claim.x as u16
            && column < claim.right() as u16
    }

    /// Adopt the transcript claim of the last painted frame. An empty claim
    /// (dialog, expanded surface, tiny frame) owns nothing.
    pub(crate) fn set_claim(&mut self, claim: Rect) {
        self.claim = claim;
    }

    /// The claim of the last painted frame, for tests.
    #[cfg(test)]
    pub(crate) fn claim(&self) -> Rect {
        self.claim
    }

    /// How far the view is scrolled up from the newest retained row.
    pub(crate) fn offset(&self) -> usize {
        self.offset_from_bottom
    }

    /// Store the quantised offset a paint derived: the split hides whole
    /// lines only, so this is at most the requested offset and also bounds
    /// the state when content shrank since the last paint.
    pub(crate) fn set_offset(&mut self, offset: usize) {
        self.offset_from_bottom = offset;
    }

    /// Scroll by `delta` physical rows: negative moves toward older content,
    /// positive back toward the bottom. Returns whether the window moved.
    ///
    /// The upper bound is not known without projecting the content, so it is
    /// clamped at paint time (see [`scroll_window_split`]); this method only
    /// clamps the bottom at follow mode.
    pub(crate) fn scroll(&mut self, delta: isize) -> bool {
        let next = if delta < 0 {
            self.offset_from_bottom.saturating_add(delta.unsigned_abs())
        } else if delta > 0 {
            self.offset_from_bottom.saturating_sub(delta as usize)
        } else {
            self.offset_from_bottom
        };
        if next == self.offset_from_bottom {
            return false;
        }
        self.offset_from_bottom = next;
        true
    }

    /// Anchor the window while scrolled up: `rows` physical rows were just
    /// committed to the retained transcript, so the same older content stays
    /// in view instead of sliding down. Follow mode stays at zero.
    pub(crate) fn anchor_committed_rows(&mut self, rows: usize) {
        if self.offset_from_bottom > 0 {
            self.offset_from_bottom += rows;
        }
    }
}

/// Split the projected retained transcript so the newest `offset` physical
/// rows are excluded.
///
/// Returns `(split_index, skipped_rows)`: `lines[..split_index]` is the
/// prefix to window over, and `skipped_rows <= offset` counts the whole lines
/// actually hidden (the split never cuts a wrapped line in half). Callers
/// store `skipped_rows` back as the honest offset, which also bounds the
/// state when content shrank since the last paint.
pub(crate) fn scroll_window_split(
    lines: &[RenderedTranscriptLine],
    width: usize,
    offset: usize,
) -> (usize, usize) {
    if offset == 0 {
        return (lines.len(), 0);
    }
    let width = width.max(1);
    let mut skipped = 0usize;
    let mut index = lines.len();
    while index > 0 {
        let rows = shadow_buffer::physical_rows(&lines[index - 1].text, width);
        if skipped + rows > offset {
            break;
        }
        skipped += rows;
        index -= 1;
    }
    (index, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> RenderedTranscriptLine {
        RenderedTranscriptLine {
            text: text.to_string(),
            ..RenderedTranscriptLine::default()
        }
    }

    fn lines(texts: &[&str]) -> Vec<RenderedTranscriptLine> {
        texts.iter().map(|text| line(text)).collect()
    }

    #[test]
    fn test_zero_offset_is_follow_mode_and_hides_nothing() {
        let all = lines(&["alpha", "beta", "gamma"]);
        let (split, skipped) = scroll_window_split(&all, 80, 0);
        assert_eq!(
            (split, skipped),
            (3, 0),
            "INVARIANT: offset 0 is follow mode — the window covers the whole \
             retained transcript; split={split} skipped={skipped}"
        );
    }

    #[test]
    fn test_scroll_window_split_excludes_the_newest_whole_lines() {
        // INVARIANT (#806): the scroll window is derived by splitting the
        // projected content at the scrolled offset, never by storing a
        // painted snapshot — a resize or disclosure change re-derives it.
        let all = lines(&["one", "two", "three", "four", "five"]);
        let (split, skipped) = scroll_window_split(&all, 80, 2);
        assert_eq!((split, skipped), (3, 2), "two newest rows hidden");
        let visible: Vec<&str> = all[..split].iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            visible,
            ["one", "two", "three"],
            "the newest two rows are excluded"
        );
    }

    #[test]
    fn test_scroll_window_split_never_cuts_a_wrapped_line() {
        // "toolongline" wraps into three physical rows at width 5; "short"
        // fits one. An offset of two cannot hide the wrapped line without
        // cutting it, so it quantises to hiding nothing; an offset of three
        // hides the whole line.
        let all = lines(&["short", "toolongline"]);
        let (split, skipped) = scroll_window_split(&all, 5, 2);
        assert_eq!(
            (split, skipped),
            (2, 0),
            "an offset that would cut a wrapped line hides nothing instead"
        );
        let (split, skipped) = scroll_window_split(&all, 5, 3);
        assert_eq!(
            (split, skipped),
            (1, 3),
            "the whole wrapped line is skipped, not cut"
        );
    }

    #[test]
    fn test_scroll_clamps_at_follow_mode_and_reports_changes() {
        let mut view = TranscriptScrollView::new();
        assert!(
            !view.scroll(3),
            "scrolling down while already following must not report a change"
        );
        assert!(
            view.scroll(-5),
            "scrolling up from follow mode moves the window"
        );
        assert_eq!(view.offset(), 5, "five rows up from the bottom");
        assert!(view.scroll(3));
        assert_eq!(view.offset(), 2, "scrolling down reduces the offset");
    }

    #[test]
    fn test_anchor_keeps_the_window_still_while_streaming_commits() {
        // INVARIANT (#806): a reader scrolled into history must not be dragged
        // by content arriving at the bottom of the conversation. Only follow
        // mode (offset 0) tracks the newest content.
        let mut view = TranscriptScrollView::new();
        view.scroll(-10);
        view.anchor_committed_rows(7);
        assert_eq!(
            view.offset(),
            17,
            "committed rows push the hidden-from-bottom count up by the same amount, \
             keeping the window anchored to the same older content"
        );
        let mut following = TranscriptScrollView::new();
        following.anchor_committed_rows(7);
        assert_eq!(
            following.offset(),
            0,
            "follow mode stays pinned to the newest content"
        );
    }

    #[test]
    fn test_ownership_follows_the_claimed_leftover_frame() {
        // INVARIANT (#806): the scroll view owns exactly the transcript claim
        // the 805 layout produced — the leftover frame under the bottom
        // chrome — and never the chrome itself.
        let mut view = TranscriptScrollView::new();
        view.set_claim(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        });
        assert!(view.owns(0, 0), "the top of the frame is transcript");
        assert!(view.owns(79, 19), "the last transcript row is owned");
        assert!(
            !view.owns(0, 20),
            "the row below the claim is bottom chrome — composer, status, or rule"
        );
        view.set_claim(Rect::default());
        assert!(
            !view.owns(0, 0),
            "a frame with no transcript claim (dialog, tiny frame) owns nothing"
        );
    }
}
