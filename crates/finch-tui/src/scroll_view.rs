//! The conversation ScrollView: renderer-owned scroll state for the transcript.
//!
//! The live transcript region — the [`super::widgets`] root column's `Flex`
//! `TRANSCRIPT` claim, the leftover frame under the bottom chrome — is a
//! scroll view over the conversation (#806). The state is presentation-only,
//! like the accordion's open set and the tool viewports' offsets: one offset
//! counted in physical terminal rows from the bottom of the scroll content,
//! where `0` is follow mode (the newest content stays in view).
//!
//! The scroll content is the **projected rendered-line union** (#897): every
//! message retained by the output port — the canonical-committed prefix and
//! the live uncommitted suffix — projected at paint time by the same
//! component-owned projection the viewport paints. The window is derived,
//! never stored: every paint splits the union at the scrolled offset and the
//! two painted surfaces divide it at the committed/live boundary (the
//! transcript region takes the committed portion, the live area the suffix's
//! lines above the hidden tail). Resize, reflow, and disclosure changes
//! therefore never desynchronize the view from the content.
//!
//! Step sizes are the ScrollView's own (#897): a wheel tick moves a few rows
//! and a PageUp/PageDown moves a page of the visible pane — never the bounded
//! tool-result viewport's one-row tick or four-row page.

use super::accordion::RenderedTranscriptLine;
use super::shadow_buffer;
use super::widgets::Rect;
use crossterm::event::MouseEventKind;

/// Rows one wheel tick moves the conversation transcript (#897).
///
/// A full transcript pane scrolls a few rows per tick; the bounded
/// tool-result viewport keeps its own one-row tick
/// (`tool_viewport::WHEEL_STEP_LINES`) and the two never mix.
pub(crate) const TRANSCRIPT_WHEEL_STEP_LINES: usize = 3;

/// The transcript's own wheel delta (#897): negative toward older content.
/// A horizontal wheel has no vertical scroll owner and stays unclaimed.
pub(crate) fn transcript_wheel_delta(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(-(TRANSCRIPT_WHEEL_STEP_LINES as isize)),
        MouseEventKind::ScrollDown => Some(TRANSCRIPT_WHEEL_STEP_LINES as isize),
        _ => None,
    }
}

/// Scroll state for the conversation transcript.
///
/// `offset_from_bottom` is how many physical rows of the newest projected
/// union are hidden below the viewport. Zero is follow mode; painting never
/// scrolls on the renderer's behalf — only input moves the offset.
#[derive(Debug, Default)]
pub(crate) struct TranscriptScrollView {
    offset_from_bottom: usize,
    /// The scroll content's physical row count as of the last derived
    /// window (#897): the anchor reads the growth since that derivation so
    /// commits and streaming appends never drag a scrolled reader.
    content_rows: usize,
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

    /// The transcript claim's visible row bounds `(top, bottom_last)` —
    /// `bottom_last` is the last visible row, inclusive (the claim's own
    /// `bottom()` is one past it). `None` when the claim is empty (a
    /// dialog or a tiny frame owns nothing to scroll or clamp against).
    pub(crate) fn visible_row_bounds(&self) -> Option<(u16, u16)> {
        if self.claim.is_empty() {
            return None;
        }
        let top = self.claim.y as u16;
        let bottom_last = (self.claim.bottom() as u16).saturating_sub(1);
        Some((top, bottom_last))
    }

    /// The drag-autoscroll delta for a point at `row` (#1237): `Some` with a
    /// negative step when `row` is at or above the claim's top edge
    /// (scrolling toward older content), a positive step when `row` is at
    /// or below the bottom edge (including rows below the claim entirely —
    /// the bottom chrome a drag can still reach), and `None` for an
    /// ordinary point inside the claim or an empty claim. `step` is the
    /// caller's own per-tick row count — production reuses
    /// [`TRANSCRIPT_WHEEL_STEP_LINES`] so drag autoscroll and wheel
    /// scrolling move at the same, already-established rate.
    pub(crate) fn drag_autoscroll_delta(&self, row: u16, step: usize) -> Option<isize> {
        let (top, bottom_last) = self.visible_row_bounds()?;
        if row <= top {
            return Some(-(step as isize));
        }
        if row >= bottom_last {
            return Some(step as isize);
        }
        None
    }

    /// How far the view is scrolled up from the newest projected row.
    pub(crate) fn offset(&self) -> usize {
        self.offset_from_bottom
    }

    /// Rows one PageUp/PageDown moves (#897): a page of the conversation
    /// pane — the transcript claim of the last painted frame — at least one
    /// row. The bounded tool-result viewport keeps its own four-row page.
    pub(crate) fn page_step(&self) -> usize {
        self.claim.height.max(1)
    }

    /// Store the quantised offset a paint derived: the split hides whole
    /// lines only, so this is at most the requested offset and also bounds
    /// the state when content shrank since the last paint. Production code
    /// positions the reader only through [`Self::scroll`] and
    /// [`Self::derive_window`]; tests use this to place the reader at an
    /// absolute offset.
    #[cfg(test)]
    pub(crate) fn set_offset(&mut self, offset: usize) {
        self.offset_from_bottom = offset;
    }

    /// Scroll by `delta` physical rows: negative moves toward older content,
    /// positive back toward the bottom. Returns whether the window moved.
    ///
    /// The upper bound is not known without projecting the content, so it is
    /// clamped at paint time (see [`scroll_window_split`] and
    /// [`Self::derive_window`]); this method only clamps the bottom at follow
    /// mode.
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

    /// Derive the paint-time window over the projected union (#806, #897).
    ///
    /// While scrolled (offset above follow mode), the offset is first
    /// anchored against the content's growth since the last derived window:
    /// rows appended at the bottom of the union — streaming appends, a turn
    /// committing — must not drag the reader, so the same older content stays
    /// in view. Follow mode (offset 0) keeps tracking the newest content and
    /// records the content size without anchoring.
    ///
    /// Returns `(split, skipped)`: `union[..split]` is the prefix above the
    /// window's hidden tail, and `skipped <= offset` counts the whole lines
    /// actually hidden (the split never cuts a wrapped line in half). The
    /// quantised `skipped` is stored back as the honest offset, which also
    /// bounds the state when content shrank since the last paint.
    pub(crate) fn derive_window(
        &mut self,
        union: &[RenderedTranscriptLine],
        width: usize,
    ) -> (usize, usize) {
        let total_rows: usize = union
            .iter()
            .map(|line| shadow_buffer::physical_rows(&line.text, width.max(1)))
            .sum();
        if self.offset_from_bottom > 0 && self.content_rows > 0 {
            let growth = total_rows.saturating_sub(self.content_rows);
            self.offset_from_bottom = self.offset_from_bottom.saturating_add(growth);
        }
        self.content_rows = total_rows;
        let (split, skipped) = scroll_window_split(union, width, self.offset_from_bottom);
        self.offset_from_bottom = skipped;
        (split, skipped)
    }
}

/// Split the projected scroll content (the live + retained union, #897) so
/// the newest `offset` physical rows are excluded.
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
    fn test_derived_window_anchors_growth_so_appends_do_not_drag_the_reader() {
        // INVARIANT (#806, #897): a reader scrolled into history must not be
        // dragged by content arriving at the bottom of the projected union —
        // streaming appends and commits alike. Only follow mode (offset 0)
        // tracks the newest content.
        let mut view = TranscriptScrollView::new();
        view.scroll(-4);
        let all = lines(&["one", "two", "three", "four", "five"]);
        let (split, skipped) = view.derive_window(&all, 80);
        assert_eq!(
            (split, skipped),
            (1, 4),
            "the first derivation over five rows hides the newest four; \
             split was {split}, skipped {skipped}"
        );
        // Three rows stream in at the bottom: the same older content stays
        // in view because the offset grows by the growth.
        let grown = lines(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ]);
        let (split, skipped) = view.derive_window(&grown, 80);
        assert_eq!(
            (split, skipped),
            (1, 7),
            "the hidden tail grew to include the appended rows, keeping the \
             window on the same older content; split was {split}, skipped {skipped}"
        );
        let visible: Vec<&str> = grown[..split]
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            visible,
            ["one"],
            "the window still ends at the same oldest row after the append"
        );
        // Scrolling back to follow mode tracks the newest content again.
        view.scroll(grown.len() as isize);
        assert_eq!(view.offset(), 0, "returning to the bottom is follow mode");
        let (split, skipped) = view.derive_window(&grown, 80);
        assert_eq!(
            (split, skipped),
            (grown.len(), 0),
            "follow mode hides nothing; split was {split}, skipped {skipped}"
        );
        // And a follow-mode append does not grow the offset.
        let (split, _) = view.derive_window(&lines(&["one", "two"]), 80);
        assert_eq!(
            split, 2,
            "follow mode keeps the whole (smaller) content in the window"
        );
        assert_eq!(view.offset(), 0, "follow mode never accumulates an offset");
    }

    #[test]
    fn test_transcript_wheel_step_is_its_own_few_lines_per_tick() {
        // INVARIANT (#897): the conversation ScrollView's wheel step is its
        // own constant, never the bounded tool-result viewport's one-row
        // tick — a full transcript pane scrolls a few rows per tick.
        assert_ne!(
            TRANSCRIPT_WHEEL_STEP_LINES,
            super::super::tool_viewport::WHEEL_STEP_LINES,
            "the transcript wheel step must differ from the tool viewport's; \
             transcript {TRANSCRIPT_WHEEL_STEP_LINES}, tool {}",
            super::super::tool_viewport::WHEEL_STEP_LINES
        );
        assert_eq!(
            transcript_wheel_delta(crossterm::event::MouseEventKind::ScrollUp),
            Some(-(TRANSCRIPT_WHEEL_STEP_LINES as isize)),
            "a wheel tick toward older content moves TRANSCRIPT_WHEEL_STEP_LINES rows"
        );
        assert_eq!(
            transcript_wheel_delta(crossterm::event::MouseEventKind::ScrollDown),
            Some(TRANSCRIPT_WHEEL_STEP_LINES as isize),
            "a wheel tick toward the bottom moves TRANSCRIPT_WHEEL_STEP_LINES rows"
        );
        assert_eq!(
            transcript_wheel_delta(crossterm::event::MouseEventKind::ScrollLeft),
            None,
            "a horizontal wheel has no vertical scroll owner"
        );
    }

    #[test]
    fn test_transcript_page_step_is_the_visible_pane_not_the_tool_page() {
        // INVARIANT (#897): PageUp/PageDown move a page of the conversation
        // pane — the last painted transcript claim — never the bounded
        // tool-result viewport's four-row page.
        let mut view = TranscriptScrollView::new();
        view.set_claim(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 17,
        });
        assert_eq!(view.page_step(), 17, "a page is the claimed pane height");
        assert_ne!(
            view.page_step(),
            super::super::tool_viewport::PAGE_STEP_LINES,
            "the transcript page step must differ from the tool viewport's; \
             transcript {}, tool {}",
            view.page_step(),
            super::super::tool_viewport::PAGE_STEP_LINES
        );
        view.set_claim(Rect::default());
        assert_eq!(
            view.page_step(),
            1,
            "a frame with no transcript claim still moves at least one row"
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

    #[test]
    fn test_drag_autoscroll_delta_fires_at_or_past_each_edge_only() {
        // INVARIANT (#1237): a drag point at or past the top/bottom edge of
        // the transcript claim must scroll toward that edge's content; a
        // point strictly inside the claim must not.
        let mut view = TranscriptScrollView::new();
        view.set_claim(Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 10,
        });
        // Claim spans rows 2..=11 (bottom() = 12, so the last visible row is 11).
        assert_eq!(
            view.drag_autoscroll_delta(2, 3),
            Some(-3),
            "at the top edge (row 2) the delta must scroll toward older content"
        );
        assert_eq!(
            view.drag_autoscroll_delta(0, 3),
            Some(-3),
            "past the top edge (row 0, above the claim entirely) still scrolls up"
        );
        assert_eq!(
            view.drag_autoscroll_delta(11, 3),
            Some(3),
            "at the bottom edge (row 11, the last visible row) the delta must scroll \
             toward newer content"
        );
        assert_eq!(
            view.drag_autoscroll_delta(15, 3),
            Some(3),
            "past the bottom edge (row 15, into the bottom chrome) still scrolls down"
        );
        assert_eq!(
            view.drag_autoscroll_delta(6, 3),
            None,
            "a point strictly inside the claim (row 6) must not autoscroll"
        );
        view.set_claim(Rect::default());
        assert_eq!(
            view.drag_autoscroll_delta(0, 3),
            None,
            "an empty claim (dialog, tiny frame) has nothing to scroll"
        );
    }

    #[test]
    fn test_visible_row_bounds_matches_the_claimed_rect() {
        let mut view = TranscriptScrollView::new();
        assert_eq!(
            view.visible_row_bounds(),
            None,
            "the default (empty) claim has no visible row bounds"
        );
        view.set_claim(Rect {
            x: 0,
            y: 5,
            width: 80,
            height: 4,
        });
        assert_eq!(
            view.visible_row_bounds(),
            Some((5, 8)),
            "a 4-row claim starting at row 5 spans rows 5..=8"
        );
    }
}
