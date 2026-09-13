//! Test-only terminal model for checking the bytes emitted by console writers.
//!
//! This deliberately knows nothing about Finch frames, layout, or shadow buffers. It models the
//! small VT surface used by the production writers after `vte` has parsed their byte stream.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum VtColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct VtStyle {
    pub foreground: VtColor,
    pub bold: bool,
    pub reverse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VtCell {
    pub character: char,
    pub style: VtStyle,
}

impl Default for VtCell {
    fn default() -> Self {
        Self {
            character: ' ',
            style: VtStyle::default(),
        }
    }
}

pub(super) struct VtOracle {
    width: usize,
    height: usize,
    cells: Vec<VtCell>,
    cursor_row: usize,
    cursor_col: usize,
    cursor_visible: bool,
    style: VtStyle,
    wrap_pending: bool,
    bytes: Vec<u8>,
    parser: Parser,
}

impl VtOracle {
    pub fn new(width: usize, height: usize) -> Self {
        assert!(
            width > 0 && height > 0,
            "VT oracle dimensions must be non-zero"
        );
        Self {
            width,
            height,
            cells: vec![VtCell::default(); width * height],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            style: VtStyle::default(),
            wrap_pending: false,
            bytes: Vec::new(),
            parser: Parser::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.advance(self, bytes);
        self.parser = parser;
    }

    pub fn cell(&self, row: usize, col: usize) -> VtCell {
        self.cells[row * self.width + col]
    }

    pub fn row(&self, row: usize) -> String {
        let start = row * self.width;
        self.cells[start..start + self.width]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    pub fn find_row(&self, needle: &str) -> Option<usize> {
        (0..self.height).find(|&row| self.row(row).contains(needle))
    }

    pub fn cursor(&self) -> (usize, usize, bool) {
        (self.cursor_row, self.cursor_col, self.cursor_visible)
    }

    pub fn diagnostic(&self) -> String {
        let mut report =
            format!(
            "terminal={}x{} cursor=({}, {}, visible={}) current_style={:?}\nbytes={:?}\nrows:\n",
            self.width,
            self.height,
            self.cursor_row,
            self.cursor_col,
            self.cursor_visible,
            self.style,
            String::from_utf8_lossy(&self.bytes).escape_debug().to_string()
        );
        for row in 0..self.height {
            let _ = writeln!(report, "{row:02}: {:?}", self.row(row));
        }
        report.push_str("styled cells:\n");
        for row in 0..self.height {
            let styled = (0..self.width)
                .filter_map(|col| {
                    let cell = self.cell(row, col);
                    (cell.character != ' ' || cell.style != VtStyle::default())
                        .then_some(format!("{col}:{:?}/{:?}", cell.character, cell.style))
                })
                .collect::<Vec<_>>();
            if !styled.is_empty() {
                let _ = writeln!(report, "{row:02}: {}", styled.join(" "));
            }
        }
        report
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cursor_row + 1 < self.height {
            self.cursor_row += 1;
            return;
        }
        self.cells.copy_within(self.width.., 0);
        let last = (self.height - 1) * self.width;
        self.cells[last..].fill(VtCell::default());
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.height - 1);
        self.cursor_col = col.min(self.width - 1);
        self.wrap_pending = false;
    }

    fn clear_line(&mut self, mode: u16) {
        let start = self.cursor_row * self.width;
        let range = match mode {
            1 => start..start + self.cursor_col + 1,
            2 => start..start + self.width,
            _ => start + self.cursor_col..start + self.width,
        };
        self.cells[range].fill(VtCell {
            character: ' ',
            style: self.style,
        });
    }

    fn clear_display(&mut self, mode: u16) {
        let cursor = self.cursor_row * self.width + self.cursor_col;
        let range = match mode {
            1 => 0..cursor + 1,
            2 | 3 => 0..self.cells.len(),
            _ => cursor..self.cells.len(),
        };
        self.cells[range].fill(VtCell {
            character: ' ',
            style: self.style,
        });
    }

    fn set_graphics(&mut self, params: &Params) {
        let values = params
            .iter()
            .map(|param| param.first().copied().unwrap_or(0))
            .collect::<Vec<_>>();
        let values = if values.is_empty() { vec![0] } else { values };
        let mut index = 0;
        while index < values.len() {
            match values[index] {
                0 => self.style = VtStyle::default(),
                1 => self.style.bold = true,
                7 => self.style.reverse = true,
                22 => self.style.bold = false,
                27 => self.style.reverse = false,
                30..=37 => self.style.foreground = VtColor::Indexed((values[index] - 30) as u8),
                38 if values.get(index + 1) == Some(&5) && values.get(index + 2).is_some() => {
                    self.style.foreground = VtColor::Indexed(values[index + 2] as u8);
                    index += 2;
                }
                38 if values.get(index + 1) == Some(&2) && values.get(index + 4).is_some() => {
                    self.style.foreground = VtColor::Rgb(
                        values[index + 2] as u8,
                        values[index + 3] as u8,
                        values[index + 4] as u8,
                    );
                    index += 4;
                }
                39 => self.style.foreground = VtColor::Default,
                90..=97 => self.style.foreground = VtColor::Indexed((values[index] - 90 + 8) as u8),
                _ => {}
            }
            index += 1;
        }
    }
}

impl Perform for VtOracle {
    fn print(&mut self, character: char) {
        let cell_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if cell_width == 0 {
            return;
        }
        if self.wrap_pending || self.cursor_col + cell_width > self.width {
            self.cursor_col = 0;
            self.linefeed();
        }
        let index = self.cursor_row * self.width + self.cursor_col;
        self.cells[index] = VtCell {
            character,
            style: self.style,
        };
        for offset in 1..cell_width {
            if self.cursor_col + offset < self.width {
                self.cells[index + offset] = VtCell {
                    character: ' ',
                    style: self.style,
                };
            }
        }
        if self.cursor_col + cell_width >= self.width {
            self.cursor_col = self.width - 1;
            self.wrap_pending = true;
        } else {
            self.cursor_col += cell_width;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.move_to(self.cursor_row, 0),
            b'\n' => self.linefeed(),
            b'\t' => {
                let next_tab_stop = (self.cursor_col / 8 + 1) * 8;
                self.move_to(self.cursor_row, next_tab_stop.min(self.width - 1));
            }
            0x08 => self.move_to(self.cursor_row, self.cursor_col.saturating_sub(1)),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let values = params
            .iter()
            .map(|param| param.first().copied().unwrap_or(0))
            .collect::<Vec<_>>();
        let value = |index: usize, default: u16| {
            values
                .get(index)
                .copied()
                .filter(|value| *value != 0)
                .unwrap_or(default) as usize
        };
        if intermediates == b"?" && values.first() == Some(&25) {
            match action {
                'h' => self.cursor_visible = true,
                'l' => self.cursor_visible = false,
                _ => {}
            }
            return;
        }
        match action {
            'A' => self.move_to(self.cursor_row.saturating_sub(value(0, 1)), self.cursor_col),
            'B' => self.move_to(self.cursor_row + value(0, 1), self.cursor_col),
            'C' => self.move_to(self.cursor_row, self.cursor_col + value(0, 1)),
            'D' => self.move_to(self.cursor_row, self.cursor_col.saturating_sub(value(0, 1))),
            'E' => self.move_to(self.cursor_row + value(0, 1), 0),
            'F' => self.move_to(self.cursor_row.saturating_sub(value(0, 1)), 0),
            'G' => self.move_to(self.cursor_row, value(0, 1).saturating_sub(1)),
            'H' | 'f' => self.move_to(value(0, 1).saturating_sub(1), value(1, 1).saturating_sub(1)),
            'J' => self.clear_display(values.first().copied().unwrap_or(0)),
            'K' => self.clear_line(values.first().copied().unwrap_or(0)),
            'm' => self.set_graphics(params),
            _ => {}
        }
    }
}
