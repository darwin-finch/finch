//! Components not yet wired into the main render path.
#![allow(dead_code)]
// Shadow Buffer - 2D character array for proper text wrapping and rendering
//
// This module implements a double-buffering system for terminal rendering:
// 1. ScrollbackBuffer (messages) → ShadowBuffer (2D chars with wrapping)
// 2. Diff ShadowBuffer with previous frame
// 3. Update only changed cells in terminal
//
// This approach ensures:
// - Long lines wrap correctly at terminal width
// - ANSI codes are preserved (zero-width)
// - No truncation or text bleeding
// - Efficient updates (only changed cells)

use crate::cli::messages::MessageRef;
use ratatui::style::Style;

/// A single cell in the shadow buffer (character + style)
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Cell {
    fn new(ch: char) -> Self {
        Self {
            ch,
            style: Style::default(),
        }
    }

    fn empty() -> Self {
        Self::new(' ')
    }

    /// Create empty cell with specific style (preserves background)
    fn empty_with_style(style: Style) -> Self {
        Self { ch: ' ', style }
    }
}

/// 2D shadow buffer for terminal rendering
pub struct ShadowBuffer {
    /// 2D array of cells [y][x]
    cells: Vec<Vec<Cell>>,
    /// Terminal width
    pub width: usize,
    /// Terminal height (scrollback area)
    pub height: usize,
}

impl ShadowBuffer {
    /// Create a new shadow buffer with given dimensions
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![vec![Cell::empty(); width]; height];
        Self {
            cells,
            width,
            height,
        }
    }

    /// Resize the buffer (called on terminal resize)
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.cells = vec![vec![Cell::empty(); width]; height];
    }

    /// Clear the buffer (reset all cells to default)
    pub fn clear(&mut self) {
        for row in &mut self.cells {
            for cell in row {
                // Reset BOTH character AND style
                *cell = Cell::empty();
            }
        }
    }

    /// Get a cell at (x, y), returns None if out of bounds
    pub fn get(&self, x: usize, y: usize) -> Option<&Cell> {
        self.cells.get(y)?.get(x)
    }

    /// Set a cell at (x, y)
    pub fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if let Some(row) = self.cells.get_mut(y) {
            if let Some(c) = row.get_mut(x) {
                *c = cell;
            }
        }
    }

    /// Write a line to the buffer at row y, handling wrapping
    /// Returns number of rows consumed
    fn write_line(&mut self, y: usize, line: &str, style: Style) -> usize {
        if y >= self.height {
            return 0;
        }

        // Split line into visible text and ANSI codes
        let (visible_chars, _ansi_positions) = extract_visible_chars(line);

        if visible_chars.is_empty() {
            return 1; // Empty line = 1 row
        }

        // Calculate how many rows this line needs
        let chars_per_row = self.width.max(1);
        let num_rows = visible_chars.len().div_ceil(chars_per_row);
        let num_rows = num_rows.min(self.height - y); // Don't exceed buffer

        // Write wrapped chunks with style
        for row_idx in 0..num_rows {
            let start = row_idx * chars_per_row;
            let end = (start + chars_per_row).min(visible_chars.len());
            let chunk = &visible_chars[start..end];

            // Write actual characters
            for (col_idx, &ch) in chunk.iter().enumerate() {
                self.set(col_idx, y + row_idx, Cell { ch, style });
            }

            // Fill remaining cells in row with spaces (but preserve background style)
            // This ensures the background extends to the full width
            for col_idx in chunk.len()..chars_per_row {
                self.set(col_idx, y + row_idx, Cell { ch: ' ', style });
            }
        }

        num_rows
    }

    /// Render messages to shadow buffer with proper wrapping
    /// Returns bottom-aligned content (last N rows that fit)
    pub fn render_messages(&mut self, messages: &[MessageRef], colors: &crate::config::ColorScheme) {
        // Clear buffer first
        self.clear();

        // Format all messages and collect lines with their styles
        let mut all_lines: Vec<(String, Style)> = Vec::new(); // (line_text, style)
        for msg in messages {
            let formatted = msg.format(colors);
            let style = msg.background_style().unwrap_or_default();
            for line in formatted.lines() {
                all_lines.push((line.to_string(), style));
            }
        }

        if all_lines.is_empty() {
            return;
        }

        // Calculate how many lines we need (with wrapping)
        let mut line_row_counts: Vec<usize> = Vec::new();

        for (line, _style) in &all_lines {
            let visible_len = visible_length(line);
            let rows = if visible_len == 0 {
                1
            } else {
                (visible_len + self.width - 1) / self.width.max(1)
            };
            line_row_counts.push(rows);
        }

        // Bottom-align: determine which lines to render
        let mut lines_to_render: Vec<(usize, &String, Style)> = Vec::new(); // (line_idx, line_text, style)
        let mut accumulated_rows = 0;

        // Walk backwards from last line
        for (line_idx, ((line, style), row_count)) in all_lines.iter().zip(&line_row_counts).enumerate().rev() {
            if accumulated_rows + row_count > self.height {
                break; // Stop when can't fit more
            }
            lines_to_render.push((line_idx, line, *style));
            accumulated_rows += row_count;
        }

        lines_to_render.reverse(); // Restore chronological order

        // Calculate starting row (bottom-aligned)
        let start_row = self.height.saturating_sub(accumulated_rows.min(self.height));

        // Render lines with their styles, handling truncation if needed
        let mut current_y = start_row;
        for (_line_idx, line, style) in lines_to_render {
            // Check if we have room left
            let rows_available = self.height.saturating_sub(current_y);
            if rows_available == 0 {
                break; // No more room
            }

            let rows_consumed = self.write_line(current_y, line, style);

            // If message was truncated (consumed more rows than available), that's ok
            // The write_line method already handles this by capping at buffer height
            current_y += rows_consumed;

            // Stop if we've filled the buffer
            if current_y >= self.height {
                break;
            }
        }
    }

    /// Get all cells as a 2D vector (for diffing)
    pub fn get_cells(&self) -> &Vec<Vec<Cell>> {
        &self.cells
    }

    /// Clone this buffer (for previous frame tracking)
    pub fn clone_buffer(&self) -> Self {
        Self {
            cells: self.cells.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

/// Calculate visible length of string (excluding ANSI escape codes)
pub fn visible_length(s: &str) -> usize {
    let mut len = 0;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Handle escape sequences
                if chars.peek() == Some(&'[') {
                    // CSI sequence: \x1b[...m (color codes, cursor movement)
                    chars.next(); // consume '['
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() {
                            break; // Sequence terminator
                        }
                    }
                } else if chars.peek() == Some(&']') {
                    // OSC sequence: \x1b]...\x07 or \x1b]...\x1b\\
                    chars.next(); // consume ']'
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' || (ch == '\x1b' && chars.peek() == Some(&'\\')) {
                            if ch == '\x1b' {
                                chars.next(); // consume '\\'
                            }
                            break;
                        }
                    }
                } else {
                    // Other escape sequences, skip 1 char
                    chars.next();
                }
            }
            '\r' | '\x08' | '\x7f' => {
                // Control characters that don't add visible length
                // \r = carriage return, \x08 = backspace, \x7f = delete
            }
            _ => {
                len += 1; // Regular visible character
            }
        }
    }

    len
}

/// Extract visible characters from string (strip ANSI codes)
/// Returns (visible_chars, positions_of_ansi_codes)
pub fn extract_visible_chars(s: &str) -> (Vec<char>, Vec<usize>) {
    let mut visible_chars = Vec::new();
    let mut ansi_positions = Vec::new();
    let mut chars = s.chars().peekable();
    let mut pos = 0;

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                ansi_positions.push(pos);
                // Skip ANSI escape sequence
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else if chars.peek() == Some(&']') {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' || (ch == '\x1b' && chars.peek() == Some(&'\\')) {
                            if ch == '\x1b' {
                                chars.next();
                            }
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\r' | '\x08' | '\x7f' => {
                // Skip control characters
            }
            _ => {
                visible_chars.push(c);
                pos += 1;
            }
        }
    }

    (visible_chars, ansi_positions)
}

/// Diff two shadow buffers and return changed cells
/// Returns Vec<(x, y, cell)> for cells that changed
pub fn diff_buffers(current: &ShadowBuffer, previous: &ShadowBuffer) -> Vec<(usize, usize, Cell)> {
    let mut changes = Vec::new();

    // If dimensions changed, return all cells
    if current.width != previous.width || current.height != previous.height {
        for y in 0..current.height {
            for x in 0..current.width {
                if let Some(cell) = current.get(x, y) {
                    changes.push((x, y, cell.clone()));
                }
            }
        }
        return changes;
    }

    // Compare cell by cell
    for y in 0..current.height {
        for x in 0..current.width {
            let curr_cell = current.get(x, y);
            let prev_cell = previous.get(x, y);

            if curr_cell != prev_cell {
                if let Some(cell) = curr_cell {
                    changes.push((x, y, cell.clone()));
                }
            }
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_length() {
        assert_eq!(visible_length("hello"), 5);
        assert_eq!(visible_length("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(visible_length(""), 0);
    }

    #[test]
    fn test_extract_visible_chars() {
        let (chars, _) = extract_visible_chars("hello");
        assert_eq!(chars, vec!['h', 'e', 'l', 'l', 'o']);

        let (chars, _) = extract_visible_chars("\x1b[31mred\x1b[0m");
        assert_eq!(chars, vec!['r', 'e', 'd']);
    }

    #[test]
    fn test_shadow_buffer_write_line() {
        let mut buf = ShadowBuffer::new(10, 5);

        // Write a short line
        let rows = buf.write_line(0, "hello", Style::default());
        assert_eq!(rows, 1);
        assert_eq!(buf.get(0, 0).unwrap().ch, 'h');
        assert_eq!(buf.get(4, 0).unwrap().ch, 'o');

        // Write a long line (should wrap)
        let rows = buf.write_line(1, "this is a very long line that wraps", Style::default());
        assert!(rows > 1);
    }

    #[test]
    fn test_diff_buffers() {
        let mut buf1 = ShadowBuffer::new(5, 3);
        let mut buf2 = ShadowBuffer::new(5, 3);

        buf1.write_line(0, "hello", Style::default());
        buf2.write_line(0, "hello", Style::default());

        // No changes
        let changes = diff_buffers(&buf1, &buf2);
        assert_eq!(changes.len(), 0);

        // Change one cell
        buf2.set(0, 0, Cell::new('x'));
        let changes = diff_buffers(&buf2, &buf1);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, 0); // x
        assert_eq!(changes[0].1, 0); // y
        assert_eq!(changes[0].2.ch, 'x');
    }

    // ─── visible_length edge cases ────────────────────────────────────────────

    #[test]
    fn test_visible_length_with_bold_and_color() {
        // Bold + color code + text + reset
        let s = "\x1b[1m\x1b[32mbold green\x1b[0m";
        assert_eq!(visible_length(s), 10); // "bold green"
    }

    #[test]
    fn test_visible_length_with_cursor_movement_codes() {
        // Cursor-movement CSI sequences should be zero-width
        let s = "\x1b[2Jhello\x1b[H"; // clear-screen + text + home
        assert_eq!(visible_length(s), 5);
    }

    #[test]
    fn test_visible_length_carriage_return_excluded() {
        let s = "hello\rworld";
        // \r doesn't add to visible length
        assert_eq!(visible_length(s), 10);
    }

    #[test]
    fn test_visible_length_pure_ansi() {
        let s = "\x1b[0m\x1b[1m\x1b[32m";
        assert_eq!(visible_length(s), 0);
    }

    #[test]
    fn test_visible_length_unicode_counts_chars() {
        // Each Unicode character counts as 1 (codepoint, not byte)
        assert_eq!(visible_length("café"), 4);
        assert_eq!(visible_length("🦀"), 1);
    }

    // ─── extract_visible_chars ───────────────────────────────────────────────

    #[test]
    fn test_extract_visible_chars_empty() {
        let (chars, positions) = extract_visible_chars("");
        assert!(chars.is_empty());
        assert!(positions.is_empty());
    }

    #[test]
    fn test_extract_visible_chars_ansi_only() {
        let (chars, positions) = extract_visible_chars("\x1b[31m\x1b[0m");
        assert!(chars.is_empty());
        assert!(!positions.is_empty()); // ansi positions recorded
    }

    #[test]
    fn test_extract_visible_chars_preserves_order() {
        let (chars, _) = extract_visible_chars("\x1b[1ma\x1b[0mb\x1b[1mc");
        assert_eq!(chars, vec!['a', 'b', 'c']);
    }

    // ─── ShadowBuffer ────────────────────────────────────────────────────────

    #[test]
    fn test_shadow_buffer_new_is_all_spaces() {
        let buf = ShadowBuffer::new(5, 3);
        for y in 0..3 {
            for x in 0..5 {
                assert_eq!(buf.get(x, y).unwrap().ch, ' ');
            }
        }
    }

    #[test]
    fn test_shadow_buffer_out_of_bounds_returns_none() {
        let buf = ShadowBuffer::new(5, 3);
        assert!(buf.get(5, 0).is_none()); // x == width
        assert!(buf.get(0, 3).is_none()); // y == height
    }

    #[test]
    fn test_shadow_buffer_clear_resets_to_spaces() {
        let mut buf = ShadowBuffer::new(5, 3);
        buf.write_line(0, "hello", Style::default());
        buf.clear();
        assert_eq!(buf.get(0, 0).unwrap().ch, ' ');
        assert_eq!(buf.get(4, 0).unwrap().ch, ' ');
    }

    #[test]
    fn test_shadow_buffer_set_and_get() {
        let mut buf = ShadowBuffer::new(5, 3);
        buf.set(2, 1, Cell::new('Z'));
        assert_eq!(buf.get(2, 1).unwrap().ch, 'Z');
    }

    #[test]
    fn test_shadow_buffer_resize_clears_content() {
        let mut buf = ShadowBuffer::new(5, 3);
        buf.write_line(0, "hello", Style::default());
        buf.resize(10, 6);
        assert_eq!(buf.width, 10);
        assert_eq!(buf.height, 6);
        // After resize everything is empty
        assert_eq!(buf.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn test_shadow_buffer_write_line_wraps_correctly() {
        let mut buf = ShadowBuffer::new(5, 10);
        let rows = buf.write_line(0, "abcdefghij", Style::default()); // 10 chars, width 5
        assert_eq!(rows, 2);
        assert_eq!(buf.get(0, 0).unwrap().ch, 'a');
        assert_eq!(buf.get(4, 0).unwrap().ch, 'e');
        assert_eq!(buf.get(0, 1).unwrap().ch, 'f');
        assert_eq!(buf.get(4, 1).unwrap().ch, 'j');
    }

    #[test]
    fn test_shadow_buffer_write_ansi_line_strips_codes() {
        let mut buf = ShadowBuffer::new(10, 5);
        // "red" with ANSI color code — should write 3 visible chars
        let rows = buf.write_line(0, "\x1b[31mred\x1b[0m", Style::default());
        assert_eq!(rows, 1);
        assert_eq!(buf.get(0, 0).unwrap().ch, 'r');
        assert_eq!(buf.get(2, 0).unwrap().ch, 'd');
    }

    #[test]
    fn test_shadow_buffer_clone_is_independent() {
        let mut buf = ShadowBuffer::new(5, 3);
        buf.write_line(0, "hello", Style::default());
        let clone = buf.clone_buffer();
        buf.clear();
        // Clone should still have 'h'
        assert_eq!(clone.get(0, 0).unwrap().ch, 'h');
    }

    // ─── diff_buffers ─────────────────────────────────────────────────────────

    #[test]
    fn test_diff_identical_buffers_no_changes() {
        let buf = ShadowBuffer::new(5, 3);
        let clone = buf.clone_buffer();
        assert_eq!(diff_buffers(&buf, &clone).len(), 0);
    }

    #[test]
    fn test_diff_dimension_mismatch_returns_all_cells() {
        let buf5x3 = ShadowBuffer::new(5, 3);
        let buf3x2 = ShadowBuffer::new(3, 2);
        let changes = diff_buffers(&buf5x3, &buf3x2);
        // When dimensions differ, all cells of current buffer returned
        assert_eq!(changes.len(), 5 * 3);
    }

    #[test]
    fn test_diff_multiple_changed_cells() {
        let mut current = ShadowBuffer::new(5, 3);
        let prev = ShadowBuffer::new(5, 3);
        current.set(0, 0, Cell::new('A'));
        current.set(1, 0, Cell::new('B'));
        current.set(2, 2, Cell::new('C'));
        let changes = diff_buffers(&current, &prev);
        assert_eq!(changes.len(), 3);
    }
}
