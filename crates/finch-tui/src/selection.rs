//! Click-drag text selection over the transcript viewport (#221).
//!
//! Finch holds mouse capture by default (#806, `mouse_capture.rs`) so wheels
//! and clicks reach the in-app widget hitboxes instead of the terminal. That
//! also means the terminal's own native click-drag text selection never
//! fires — #221 tracked this as a known trade. This module is Finch's own
//! in-app replacement: it tracks a drag over the transcript, highlights the
//! dragged range, and copies the released selection to the system clipboard.
//!
//! **Scope (first slice).** [`SelectionIndex`] only indexes transcript rows
//! that occupy exactly one physical terminal row
//! (`finch_ui_model::physical_rows(text, width) == 1`). A row produced by a
//! wrapped multi-row logical line is not indexed, so it cannot be selected,
//! highlighted, or copied — a drag simply has a gap there. Most transcript
//! content (status lines, single-sentence prose, code lines, user input
//! lines) is single-row, so this covers the common case; wrapping-aware
//! selection is a documented follow-up rather than something this module
//! guesses at.
//!
//! Column math (`column_to_char_index`) walks `text` char-by-char using
//! display width, with no awareness of embedded ANSI escapes. Most
//! transcript lines are plain by the time they reach here (component spans
//! are lowered separately, at paint), but a legacy line that still carries
//! raw SGR bytes directly in its `text` field (span-free, pre-formatted
//! content — see `span_render::lower_rendered_line`'s doc comment) would
//! have its escape bytes miscounted as display columns. That is a known,
//! narrow gap in this first slice rather than something silently
//! mishandled: such a line still selects and copies (whole-row column
//! bounds are unaffected), only a partial-column boundary landing inside
//! its escape bytes could drift.
//!
//! The index is rebuilt every frame in
//! `TuiRenderer::rebuild_transcript_hit_regions`, from the same
//! `RenderedTranscriptLine`s the accordion hit regions use, so it is always
//! consistent with what is actually on screen. A selection is cleared
//! whenever the transcript is fully repainted (`redraw_full_viewport_inner`:
//! a new committed message, an explicit scroll, or a terminal resize) —
//! see that function's doc comment for why a simple "clear on redraw" rule
//! was chosen over trying to carry a selection across content that moved.

use finch_ui_model::{char_display_width, physical_rows, RenderedTranscriptLine};

/// One selectable, single-physical-row transcript line, keyed by its
/// absolute terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectableRow {
    pub row: u16,
    pub text: String,
}

/// Per-frame snapshot of the rows currently on screen that a drag can
/// select. See the module docs for why only single-physical-row lines are
/// indexed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SelectionIndex {
    rows: Vec<SelectableRow>,
}

impl SelectionIndex {
    /// Build the index from the same ordered `RenderedTranscriptLine`s and
    /// starting row the transcript hit-region rebuild uses.
    pub fn build(combined: &[RenderedTranscriptLine], top: u16, width: u16) -> Self {
        let width = usize::from(width).max(1);
        let mut rows = Vec::new();
        let mut cursor = usize::from(top);
        for line in combined {
            let rows_here = physical_rows(&line.text, width);
            if rows_here == 1 && cursor <= usize::from(u16::MAX) {
                rows.push(SelectableRow {
                    row: cursor as u16,
                    text: line.text.clone(),
                });
            }
            cursor += rows_here;
        }
        Self { rows }
    }

    pub fn row(&self, row: u16) -> Option<&SelectableRow> {
        self.rows.iter().find(|entry| entry.row == row)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// Map an absolute display column to a char index into `text`: the index of
/// the character occupying that column, or `text`'s length in chars if the
/// column is past the end. Wide characters (CJK, emoji) occupy two columns;
/// a column that lands on the right half of one snaps to the character's
/// own index (the whole character is included or excluded together).
pub(crate) fn column_to_char_index(text: &str, column: u16) -> usize {
    let column = usize::from(column);
    let mut width_so_far = 0usize;
    for (idx, ch) in text.chars().enumerate() {
        let width = char_display_width(ch).max(1);
        // `column` falls inside this char's own span
        // [width_so_far, width_so_far + width) — including the right half of
        // a wide character, which must snap to the wide char's index rather
        // than the next one.
        if column < width_so_far + width {
            return idx;
        }
        width_so_far += width;
    }
    text.chars().count()
}

/// One endpoint of a drag, in absolute terminal coordinates (matching
/// `crossterm::event::MouseEvent::{row, column}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SelectionPoint {
    pub row: u16,
    pub col: u16,
}

/// A click-drag text selection over the transcript.
///
/// `dragging` is true from the first `Drag` event until `Up` finalizes it.
/// A finalized selection stays visually highlighted and copyable until
/// something clears it — a new press-drag, a scroll, a resize, or a new
/// committed message (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptSelection {
    pub anchor: SelectionPoint,
    pub head: SelectionPoint,
    pub dragging: bool,
}

impl TranscriptSelection {
    pub fn new(anchor: SelectionPoint) -> Self {
        Self {
            anchor,
            head: anchor,
            dragging: true,
        }
    }

    pub fn extend(&mut self, to: SelectionPoint) {
        self.head = to;
    }

    pub fn finish(&mut self) {
        self.dragging = false;
    }

    /// Normalized inclusive row range, top first.
    pub fn row_range(&self) -> (u16, u16) {
        if self.anchor.row <= self.head.row {
            (self.anchor.row, self.head.row)
        } else {
            (self.head.row, self.anchor.row)
        }
    }

    /// True when anchor and head are the same point — a click that never
    /// moved, not a drag.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// The inclusive display-column range selected on `row`, or `None` when
/// `row` falls outside the selection entirely. `u16::MAX` as the high bound
/// means "to the end of the line" (the row's own text length decides where
/// that actually is).
fn row_col_bounds(selection: &TranscriptSelection, row: u16) -> Option<(u16, u16)> {
    let (top, bottom) = selection.row_range();
    if row < top || row > bottom {
        return None;
    }
    if top == bottom {
        let lo = selection.anchor.col.min(selection.head.col);
        let hi = selection.anchor.col.max(selection.head.col);
        return Some((lo, hi));
    }
    if row == top {
        let col = if selection.anchor.row == top {
            selection.anchor.col
        } else {
            selection.head.col
        };
        return Some((col, u16::MAX));
    }
    if row == bottom {
        let col = if selection.anchor.row == bottom {
            selection.anchor.col
        } else {
            selection.head.col
        };
        return Some((0, col));
    }
    Some((0, u16::MAX))
}

/// Convert a row's inclusive column bound into an exclusive char-index range
/// into its text, clamped to the text's own length.
fn char_range(text: &str, start_col: u16, end_col: u16) -> (usize, usize) {
    let char_count = text.chars().count();
    let start = column_to_char_index(text, start_col);
    let end = if end_col == u16::MAX {
        char_count
    } else {
        column_to_char_index(text, end_col.saturating_add(1))
    };
    let start = start.min(char_count);
    let end = end.clamp(start, char_count);
    (start, end)
}

/// The plain text of a finalized or in-progress selection, one line per
/// selected row joined with `\n`. A row the index has no entry for (a hit
/// region, chrome, or a wrapped line — see the module docs) is skipped
/// rather than guessed at.
pub(crate) fn selected_text(index: &SelectionIndex, selection: &TranscriptSelection) -> String {
    let (top, bottom) = selection.row_range();
    let mut lines = Vec::new();
    for row in top..=bottom {
        let Some(entry) = index.row(row) else {
            continue;
        };
        let Some((start_col, end_col)) = row_col_bounds(selection, row) else {
            continue;
        };
        let (start, end) = char_range(&entry.text, start_col, end_col);
        if start >= end {
            // A real row with nothing selected on it (e.g. an empty line
            // fully inside the range) still contributes a blank line, so
            // multi-line copies keep their line breaks.
            lines.push(String::new());
            continue;
        }
        lines.push(entry.text.chars().skip(start).take(end - start).collect());
    }
    lines.join("\n")
}

/// Rows to paint with the highlight background for the current selection:
/// `(absolute row, full line text, inclusive-exclusive char range to
/// highlight)`. Empty (zero-width) selections paint nothing — a plain click
/// never shows a highlight.
pub(crate) fn highlighted_rows(
    index: &SelectionIndex,
    selection: &TranscriptSelection,
) -> Vec<(u16, String, (usize, usize))> {
    if selection.is_empty() {
        return Vec::new();
    }
    let (top, bottom) = selection.row_range();
    let mut out = Vec::new();
    for row in top..=bottom {
        let Some(entry) = index.row(row) else {
            continue;
        };
        let Some((start_col, end_col)) = row_col_bounds(selection, row) else {
            continue;
        };
        let (start, end) = char_range(&entry.text, start_col, end_col);
        if start >= end {
            continue;
        }
        out.push((row, entry.text.clone(), (start, end)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain rendered line. `RenderedTranscriptLine` carries no row of its
    /// own — `SelectionIndex::build` derives each line's absolute row by
    /// walking `physical_rows` from its `top` argument — so the `row`
    /// parameter here is unused and only documents which row the caller
    /// expects this line to land on once indexed.
    fn line(_row: u16, text: &str) -> RenderedTranscriptLine {
        RenderedTranscriptLine {
            text: text.to_string(),
            ..RenderedTranscriptLine::default()
        }
    }

    #[test]
    fn test_column_to_char_index_walks_wide_characters_by_display_width() {
        // "a" (1 col) + "文" (2 cols, CJK) + "b" (1 col): columns 0,1,2,3.
        let text = "a文b";
        assert_eq!(column_to_char_index(text, 0), 0, "column 0 is 'a'");
        assert_eq!(
            column_to_char_index(text, 1),
            1,
            "column 1 is the wide char's own start"
        );
        assert_eq!(
            column_to_char_index(text, 2),
            1,
            "column 2 lands on the wide char's right half; snaps to its index, not past it"
        );
        assert_eq!(column_to_char_index(text, 3), 2, "column 3 is 'b'");
        assert_eq!(
            column_to_char_index(text, 99),
            3,
            "a column past the end clamps to the char count"
        );
    }

    #[test]
    fn test_selection_index_skips_wrapped_multi_row_lines() {
        let combined = vec![
            line(0, "short"),
            RenderedTranscriptLine {
                text: "x".repeat(50),
                ..RenderedTranscriptLine::default()
            },
            line(0, "also short"),
        ];
        let index = SelectionIndex::build(&combined, 5, 20);
        // Row 5 = "short" (1 row), row 6 = the 50-char line wraps to 3 rows
        // at width 20 (not indexed), row 9 = "also short".
        assert_eq!(
            index.len(),
            2,
            "the wrapped middle line must not be indexed; got {} entries",
            index.len()
        );
        assert_eq!(index.row(5).map(|r| r.text.as_str()), Some("short"));
        assert_eq!(
            index.row(6),
            None,
            "row 6 starts the wrapped line — no entry"
        );
        assert_eq!(index.row(9).map(|r| r.text.as_str()), Some("also short"));
    }

    #[test]
    fn test_selected_text_single_row_partial_columns_forward_drag() {
        let index = SelectionIndex::build(&[line(0, "hello world")], 3, 80);
        let selection = TranscriptSelection {
            anchor: SelectionPoint { row: 3, col: 0 },
            head: SelectionPoint { row: 3, col: 4 },
            dragging: false,
        };
        assert_eq!(
            selected_text(&index, &selection),
            "hello",
            "columns 0..=4 of 'hello world' is 'hello'"
        );
    }

    #[test]
    fn test_selected_text_single_row_reversed_drag_normalizes_columns() {
        // Dragging right-to-left: head.col < anchor.col on the same row.
        let index = SelectionIndex::build(&[line(0, "hello world")], 3, 80);
        let selection = TranscriptSelection {
            anchor: SelectionPoint { row: 3, col: 4 },
            head: SelectionPoint { row: 3, col: 0 },
            dragging: false,
        };
        assert_eq!(
            selected_text(&index, &selection),
            "hello",
            "reversed drag on one row must normalize to the same forward range"
        );
    }

    #[test]
    fn test_selected_text_multi_row_spans_full_and_partial_lines() {
        let index = SelectionIndex::build(&[line(0, "first line"), line(0, "second line")], 3, 80);
        let selection = TranscriptSelection {
            anchor: SelectionPoint { row: 3, col: 6 },
            head: SelectionPoint { row: 4, col: 5 },
            dragging: false,
        };
        assert_eq!(
            selected_text(&index, &selection),
            "line\nsecond",
            "top row keeps its tail from the anchor column (6, the space before \
             'line'); bottom row keeps its head up to and including the drag \
             column (5, the 'd' of 'second')"
        );
    }

    #[test]
    fn test_selected_text_skips_rows_the_index_has_no_entry_for() {
        // Row 4 is a gap (e.g. a hit-region row, or a wrapped line) between
        // two selectable rows; the selection still produces the two real
        // lines without inventing content for the gap.
        let combined = [line(0, "top"), line(0, "bottom")];
        let mut index = SelectionIndex::build(&combined[..1], 3, 80);
        let bottom_index = SelectionIndex::build(&combined[1..], 5, 80);
        index.rows.extend(bottom_index.rows);
        let selection = TranscriptSelection {
            anchor: SelectionPoint { row: 3, col: 0 },
            head: SelectionPoint { row: 5, col: 6 },
            dragging: false,
        };
        assert_eq!(selected_text(&index, &selection), "top\nbottom");
    }

    #[test]
    fn test_click_without_drag_selects_nothing() {
        let index = SelectionIndex::build(&[line(0, "hello")], 3, 80);
        let selection = TranscriptSelection::new(SelectionPoint { row: 3, col: 2 });
        assert!(selection.is_empty(), "anchor == head before any Drag event");
        assert!(
            highlighted_rows(&index, &selection).is_empty(),
            "a plain click must not paint a highlight"
        );
    }

    #[test]
    fn test_highlighted_rows_reports_char_range_for_partial_row() {
        let index = SelectionIndex::build(&[line(0, "hello world")], 3, 80);
        let mut selection = TranscriptSelection::new(SelectionPoint { row: 3, col: 0 });
        selection.extend(SelectionPoint { row: 3, col: 4 });
        let rows = highlighted_rows(&index, &selection);
        assert_eq!(
            rows,
            vec![(3, "hello world".to_string(), (0, 5))],
            "highlighting 'hello' is char range [0,5) into the full row text"
        );
    }
}
