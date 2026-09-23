//! The wizard widget host: the setup wizard's screens on the claiming widget tree.
//!
//! #812: the setup wizard is no longer a second ratatui app. Its screen is one
//! owned [`WizardView`] snapshot — tab row, section lines, help, and an
//! optional overlay card — projected into the same claiming widget tree the
//! live console uses ([`widgets::layout`]), planned into a [`WizardFrame`]
//! ("one planner, two consumers"), recorded in a [`ShadowBuffer`], and blitted
//! to the terminal by row diff. Overlay cards are the #807 dialog-card
//! contract: a claimed rect whose chrome — title and controls — is pinned
//! inside the card, never a floating second painter and never the general
//! z-compositor (#793 stays a follow-up).
//!
//! The host owns no terminal lifecycle. Entering and leaving raw mode, the
//! alternate screen, and mouse capture stay with the wizard's caller so the
//! #265 handoff contract is untouched; the host only turns frames into bytes.
//! The GUI host (#808) can consume the same `WizardView` props without any of
//! this file.

use std::io::Write;

use super::shadow_buffer::ShadowBuffer;
use super::widgets::{self, Axis, Rect, Track, Widget};
use anyhow::Result;
use crossterm::{
    cursor::Hide,
    execute,
    style::Print,
    terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};
use finch_ui_model::char_display_width;

// ─── Styling ─────────────────────────────────────────────────────────────────
//
// Stage 4 (#1141): wizard view props carry **styled spans** — semantic
// (text, style) segments with no terminal bytes. The spans are built here and
// by the wizard's view builders from the `WizardColor` vocabulary; the host
// lowers them to SGR exactly once, in `lower_wizard_line`, when a frame is
// planned. The shadow buffer strips the codes for measurement, so styling can
// never change a frame's geometry, and a plain span paints byte-identically
// to the pre-span text.

/// The wizard view's own colour vocabulary.
///
/// Deliberately not the renderer's colour type: a GUI host (#808) consumes
/// `WizardView` too, and a plain named-colour enum plus truecolour carry the
/// same meaning without binding the view to any one renderer's palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    White,
    /// A theme colour by value; the themes render with truecolour SGR.
    Truecolor {
        r: u8,
        g: u8,
        b: u8,
    },
}

/// One styled wizard segment: optional foreground/background colour plus the
/// bold and dim modifiers. No escape sequences here — the lowering is the
/// host's job, and a GUI consumer reads the colours directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardSpan {
    pub text: String,
    pub fg: Option<WizardColor>,
    pub bg: Option<WizardColor>,
    pub bold: bool,
    pub dim: bool,
}

impl WizardSpan {
    /// A plain (unstyled) span.
    pub fn plain(text: impl Into<String>) -> WizardSpan {
        WizardSpan {
            text: text.into(),
            fg: None,
            bg: None,
            bold: false,
            dim: false,
        }
    }

    /// A coloured, optionally bold span.
    pub fn styled(text: impl Into<String>, fg: Option<WizardColor>, bold: bool) -> WizardSpan {
        WizardSpan {
            text: text.into(),
            fg,
            bg: None,
            bold,
            dim: false,
        }
    }

    /// A span with a background (the #1140 selection contrast channel).
    pub fn with_background(
        text: impl Into<String>,
        fg: WizardColor,
        bg: WizardColor,
    ) -> WizardSpan {
        WizardSpan {
            text: text.into(),
            fg: Some(fg),
            bg: Some(bg),
            bold: true,
            dim: false,
        }
    }
}

/// One logical wizard line: styled segments in paint order. A line's display
/// width is the sum of its segments' visible widths, so styling can never
/// shift a frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WizardLine(pub Vec<WizardSpan>);

impl WizardLine {
    /// A one-span plain line.
    pub fn plain(text: impl Into<String>) -> WizardLine {
        WizardLine(vec![WizardSpan::plain(text)])
    }

    /// The blank line — zero segments, zero columns.
    pub fn blank() -> WizardLine {
        WizardLine(Vec::new())
    }

    /// A coloured, non-bold one-span line.
    pub fn colored(text: impl Into<String>, fg: WizardColor) -> WizardLine {
        WizardLine(vec![WizardSpan::styled(text, Some(fg), false)])
    }

    /// A coloured, bold one-span line.
    pub fn bold(text: impl Into<String>, fg: WizardColor) -> WizardLine {
        WizardLine(vec![WizardSpan::styled(text, Some(fg), true)])
    }

    /// Join lines left to right into one logical line.
    pub fn concat(lines: &[WizardLine]) -> WizardLine {
        let mut spans = Vec::new();
        for line in lines {
            spans.extend(line.0.iter().cloned());
        }
        WizardLine(spans)
    }

    /// The line's plain text — the concatenation of its segment texts, the
    /// speakable form a screen reader or a GUI host reads.
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for span in &self.0 {
            text.push_str(&span.text);
        }
        text
    }

    /// Visible display columns the line occupies (ANSI-free by construction).
    pub fn display_length(&self) -> usize {
        self.0.iter().map(wizard_span_visible_length).sum()
    }

    /// Physical terminal rows this logical line occupies at `width`.
    pub fn physical_rows(&self, width: usize) -> usize {
        self.display_length().max(1).div_ceil(width.max(1))
    }
}

/// Visible display-column width of one span's text.
fn wizard_span_visible_length(span: &WizardSpan) -> usize {
    span.text.chars().map(wizard_char_width).sum()
}

/// SGR reset closing every styled wizard span.
const WIZ_RESET: &str = "\x1b[0m";

// ─── Terminal-accurate measurement (#926) ────────────────────────────────────
//
// The wizard's frames are blitted by row diff: what the planner thinks a row
// occupies must equal what the terminal actually renders, or a wrapped row
// shifts everything below it and the diff starts repainting the wrong rows.
// The shared vocabulary's `char_display_width` counts emoji-presentation
// codepoints as one column; xterm-class terminals render them as two, and the
// wizard's own content (theme previews, feature checkboxes) uses them. These
// wizard-local measurements are the ones every wizard line builder, the frame
// planner, and the view projection must agree on.

/// Terminal display width of one character as xterm-class terminals render it.
///
/// The vocabulary's CJK/fullwidth ranges plus the emoji-presentation
/// codepoints a terminal paints double-width.
fn wizard_char_width(c: char) -> usize {
    match c as u32 {
        // Emoji presentation (East Asian Width Wide / double-width emoji):
        // the wizard's own content uses 🔧 ❌ ✅ 🧠.
        0x231A..=0x231B
        | 0x2614
        | 0x2615
        | 0x2705
        | 0x270A..=0x270B
        | 0x2728
        | 0x274C
        | 0x274E
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27B0
        | 0x27BF
        | 0x2B1B..=0x2B1C
        | 0x2B50
        | 0x2B55
        | 0x1F000..=0x1FAFF => 2,
        _ => char_display_width(c),
    }
}

/// Visible display-column width of `text` as a terminal renders it
/// (ANSI-stripped, emoji-aware). Wizard line builders must pad and centre
/// with this, not with the engine's narrower measure.
pub fn wizard_visible_length(text: &str) -> usize {
    let mut len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
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
            '\r' | '\x08' | '\x7f' => {}
            _ => len += wizard_char_width(c),
        }
    }
    len
}

/// Physical terminal rows `text` occupies at `width` as a terminal renders it.
pub fn wizard_physical_rows(text: &str, width: usize) -> usize {
    wizard_visible_length(text).max(1).div_ceil(width.max(1))
}

/// The boxed-region top border: `┌─ {title} ` followed by a `─` glyph run and
/// `┐`, exactly `width` visible columns (#926: the gap count must be glyphs,
/// not digits, and the run must fill the row — the frame's row-diff depends on
/// every border row being exactly one terminal row).
fn wizard_title_border(title: &str, width: usize, accent: WizardColor, bold: bool) -> WizardLine {
    if width < 5 {
        // A row too small for a title still closes the box without overflow.
        let dashes = width.saturating_sub(2);
        let glyphs = if width == 1 {
            "┌".to_string()
        } else if width == 0 {
            String::new()
        } else {
            format!("┌{}┐", "─".repeat(dashes))
        };
        return wizard_paint(&glyphs, Some(accent), bold);
    }
    // `┌─ ` + title + ` ` + dashes + `┐` = exactly `width` columns.
    let title_budget = width - 5;
    let mut fitted = String::new();
    for ch in title.chars() {
        let next = wizard_word_width(&format!("{fitted}{ch}"));
        if next > title_budget {
            break;
        }
        fitted.push(ch);
    }
    let dashes = width - 5 - wizard_word_width(&fitted);
    wizard_paint(
        &format!("┌─ {fitted} {}┐", "─".repeat(dashes)),
        Some(accent),
        bold,
    )
}

impl From<ratatui::style::Color> for WizardColor {
    fn from(color: ratatui::style::Color) -> Self {
        use ratatui::style::Color as Renderer;
        match color {
            Renderer::Black => Self::Black,
            Renderer::Red | Renderer::LightRed => Self::Red,
            Renderer::Green | Renderer::LightGreen => Self::Green,
            Renderer::Yellow | Renderer::LightYellow => Self::Yellow,
            Renderer::Blue | Renderer::LightBlue => Self::Blue,
            Renderer::Magenta | Renderer::LightMagenta => Self::Magenta,
            Renderer::Cyan | Renderer::LightCyan => Self::Cyan,
            Renderer::Gray => Self::Gray,
            Renderer::DarkGray => Self::DarkGray,
            Renderer::White => Self::White,
            Renderer::Rgb(r, g, b) => Self::Truecolor { r, g, b },
            _ => Self::Gray,
        }
    }
}

fn sgr_fg(color: WizardColor) -> String {
    match color {
        WizardColor::Black => "30".to_string(),
        WizardColor::Red => "31".to_string(),
        WizardColor::Green => "32".to_string(),
        WizardColor::Yellow => "33".to_string(),
        WizardColor::Blue => "34".to_string(),
        WizardColor::Magenta => "35".to_string(),
        WizardColor::Cyan => "36".to_string(),
        WizardColor::Gray => "37".to_string(),
        WizardColor::DarkGray => "90".to_string(),
        WizardColor::White => "97".to_string(),
        WizardColor::Truecolor { r, g, b } => format!("38;2;{r};{g};{b}"),
    }
}

fn sgr_bg(color: WizardColor) -> String {
    match color {
        WizardColor::Black => "40".to_string(),
        WizardColor::Red => "41".to_string(),
        WizardColor::Green => "42".to_string(),
        WizardColor::Yellow => "43".to_string(),
        WizardColor::Blue => "44".to_string(),
        WizardColor::Magenta => "45".to_string(),
        WizardColor::Cyan => "46".to_string(),
        WizardColor::Gray => "47".to_string(),
        WizardColor::DarkGray => "100".to_string(),
        WizardColor::White => "107".to_string(),
        WizardColor::Truecolor { r, g, b } => format!("48;2;{r};{g};{b}"),
    }
}

/// The SGR attribute run for one span: bold, dim, foreground, background —
/// the same attribute order the pre-span helpers emitted (bold first, then
/// colour), with the background appended.
fn span_sgr_codes(span: &WizardSpan) -> String {
    let mut codes: Vec<String> = Vec::new();
    if span.bold {
        codes.push("1".to_string());
    }
    if span.dim {
        codes.push("2".to_string());
    }
    if let Some(fg) = span.fg {
        codes.push(sgr_fg(fg));
    }
    if let Some(bg) = span.bg {
        codes.push(sgr_bg(bg));
    }
    codes.join(";")
}

/// THE LOWERING (stage 4 #1141): turn one styled wizard span into the bytes a
/// terminal paints. This is the only place in the wizard surface that names
/// escape codes; view builders construct spans, never bytes.
pub fn lower_wizard_span(span: &WizardSpan) -> String {
    let codes = span_sgr_codes(span);
    if codes.is_empty() {
        span.text.clone()
    } else {
        format!("\x1b[{codes}m{}{WIZ_RESET}", span.text)
    }
}

/// Lower one logical line to its painted bytes.
pub fn lower_wizard_line(line: &WizardLine) -> String {
    let mut out = String::new();
    for span in &line.0 {
        out.push_str(&lower_wizard_span(span));
    }
    out
}

/// The selection style (#1140): bold bright-white on a black background —
/// the pre-migration `bg(Black).fg(White)` contrast the widget-host migration
/// lost, restored through the background channel so a selected row reads on
/// any terminal background.
pub fn wizard_selected(text: impl Into<String>) -> WizardLine {
    WizardLine(vec![WizardSpan::with_background(
        text,
        WizardColor::White,
        WizardColor::Black,
    )])
}

/// True when the line marks a wizard selection (the #1140 contrast prop).
pub fn wizard_line_is_selected(line: &WizardLine) -> bool {
    line.0.iter().any(|span| span.bg.is_some() && span.bold)
}

/// One styled wizard line: optional foreground colour plus bold —
/// pre-migration shape, now as spans instead of baked bytes.
pub fn wizard_paint(text: &str, fg: Option<WizardColor>, bold: bool) -> WizardLine {
    WizardLine(vec![WizardSpan::styled(text, fg, bold)])
}

/// A coloured, non-bold wizard span.
pub fn wizard_line(text: &str, fg: WizardColor) -> WizardLine {
    WizardLine::colored(text, fg)
}

/// A coloured, bold wizard span.
pub fn wizard_bold(text: &str, fg: WizardColor) -> WizardLine {
    WizardLine::bold(text, fg)
}

/// A plain wizard span.
pub fn wizard_plain(text: &str) -> WizardLine {
    WizardLine::plain(text)
}

/// Centre a styled line in `width` display columns (a leading plain pad).
pub fn wizard_centered(line: WizardLine, width: usize) -> WizardLine {
    let lead = width.saturating_sub(line.display_length()) / 2;
    let mut spans = Vec::with_capacity(line.0.len() + 1);
    if lead > 0 {
        spans.push(WizardSpan::plain(" ".repeat(lead)));
    }
    spans.extend(line.0);
    WizardLine(spans)
}

/// Word-wrap a styled line at `width` display columns. Each fragment
/// re-carries the style of the segment it started in, so wrapped box rows
/// keep their style — the span equivalent of the SGR-prefix behaviour the
/// string wrapper had. An embedded `\n` breaks the line hard, so user text
/// (a persona's system prompt) can never feed the terminal a raw linefeed
/// mid-row.
///
/// A boxed body must wrap like the old painter's `Paragraph`, not truncate —
/// truncation is exactly how an accessibility string loses its last words.
pub fn wizard_wrap(line: &WizardLine, width: usize) -> Vec<WizardLine> {
    let width = width.max(1);
    let mut lines_out: Vec<WizardLine> = Vec::new();
    let mut current: Vec<WizardSpan> = Vec::new();
    let mut column = 0usize;

    for span in &line.0 {
        // Hard breaks first: an embedded linefeed must never reach the frame
        // as a raw byte (#926); it ends the logical row where it stands.
        for (paragraph_index, paragraph) in span.text.split('\n').enumerate() {
            if paragraph_index > 0 {
                close_row(&mut current, &mut lines_out);
                column = 0;
            }
            let paragraph = paragraph.trim_end_matches('\r');
            for word in paragraph.split(' ') {
                if column > 0 && column + wizard_word_width(word) > width {
                    close_row(&mut current, &mut lines_out);
                    column = 0;
                }
                for ch in word.chars() {
                    let char_width = wizard_char_width(ch);
                    if column + char_width > width {
                        close_row(&mut current, &mut lines_out);
                        column = 0;
                    }
                    append_char(&mut current, span, ch);
                    column += char_width;
                }
                // The separator space rides the word's own style; the row
                // trim removes it when it lands at the end.
                append_char(&mut current, span, ' ');
                column += 1;
            }
        }
    }
    if !current.is_empty() {
        close_row(&mut current, &mut lines_out);
    } else if lines_out.is_empty() {
        lines_out.push(WizardLine::blank());
    }
    lines_out
}

fn close_row(current: &mut Vec<WizardSpan>, lines_out: &mut Vec<WizardLine>) {
    while let Some(last) = current.last_mut() {
        let trimmed = last.text.trim_end().to_string();
        if trimmed.is_empty() {
            current.pop();
        } else {
            last.text = trimmed;
            break;
        }
    }
    lines_out.push(WizardLine(std::mem::take(current)));
}

/// Greedy word width as a terminal renders it.
fn wizard_word_width(word: &str) -> usize {
    word.chars().map(wizard_char_width).sum()
}

fn same_style(a: &WizardSpan, b: &WizardSpan) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.bold == b.bold && a.dim == b.dim
}

/// Append one character to the line, merging with the previous span when the
/// styles agree so contiguous same-style text stays one segment.
fn append_char(current: &mut Vec<WizardSpan>, style_of: &WizardSpan, ch: char) {
    match current.last_mut() {
        Some(last) if same_style(last, style_of) => last.text.push(ch),
        _ => current.push(WizardSpan {
            text: ch.to_string(),
            fg: style_of.fg,
            bg: style_of.bg,
            bold: style_of.bold,
            dim: style_of.dim,
        }),
    }
}

/// One boxed region of a wizard section: `title` on the top border, every
/// body line wrapped and padded so the accent border stays on the box.
pub fn wizard_boxed(
    title: &str,
    body: &[WizardLine],
    accent: WizardColor,
    width: usize,
) -> Vec<WizardLine> {
    let width = width.max(4);
    let inner = width - 2;
    let border = "─".repeat(inner);
    let mut out = vec![wizard_title_border(title, width, accent, false)];
    for line in body {
        for fragment in wizard_wrap(line, inner.saturating_sub(2)) {
            let pad = inner
                .saturating_sub(2)
                .saturating_sub(fragment.display_length());
            let mut spans = vec![
                WizardSpan::styled("│", Some(accent), false),
                WizardSpan::plain(" "),
            ];
            spans.extend(fragment.0);
            spans.push(WizardSpan::plain(format!(" {}", " ".repeat(pad))));
            spans.push(WizardSpan::styled("│", Some(accent), false));
            out.push(WizardLine(spans));
        }
    }
    out.push(wizard_paint(&format!("└{border}┘"), Some(accent), false));
    out
}

// ─── Overlay cards: the #807 dialog-card contract ────────────────────────────

/// One overlay card: a claimed rect whose chrome is pinned inside it.
///
/// `controls` is the keyboard affordance row (`Esc: Cancel` and friends); it
/// is pinned above the bottom border for the same reason `pin_dialog_controls`
/// pins Yes/No in a conversation dialog — a long body can never push the way
/// out of the card.
pub struct WizardCard {
    pub title: String,
    pub body: Vec<WizardLine>,
    pub controls: Option<WizardLine>,
    pub accent: WizardColor,
}

impl WizardCard {
    /// A card that announces and instructs: title, body, controls, cyan chrome.
    pub fn new(
        title: impl Into<String>,
        body: Vec<WizardLine>,
        controls: Option<WizardLine>,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            controls,
            accent: WizardColor::Cyan,
        }
    }

    /// The card's controls row as the host paints it: yellow, the way the old
    /// painter styled every controls row. The props carry the text; the chrome
    /// style stays the host's.
    fn controls_line(&self) -> Option<WizardLine> {
        self.controls
            .as_ref()
            .map(|controls| WizardLine::colored(controls.plain_text(), WizardColor::Yellow))
    }

    /// The body (controls pinned last) wrapped to one fragment per row, the
    /// unit the chrome and the pinned rebuild both count.
    fn wrapped_body(&self, width: usize) -> Vec<WizardLine> {
        let width = width.max(4);
        let inner = width - 2;
        let mut body: Vec<WizardLine> = self.body.clone();
        if let Some(controls) = self.controls_line() {
            body.push(controls);
        }
        let mut fragments = Vec::new();
        for line in &body {
            fragments.extend(wizard_wrap(line, inner.saturating_sub(2)));
        }
        fragments
    }

    fn boxed_fragment(&self, width: usize, fragment: &WizardLine) -> WizardLine {
        let width = width.max(4);
        let inner = width - 2;
        let pad = inner
            .saturating_sub(2)
            .saturating_sub(fragment.display_length());
        let mut spans = vec![
            WizardSpan::styled("│", Some(self.accent), false),
            WizardSpan::plain(" "),
        ];
        spans.extend(fragment.0.clone());
        spans.push(WizardSpan::plain(format!(" {}", " ".repeat(pad))));
        spans.push(WizardSpan::styled("│", Some(self.accent), false));
        WizardLine(spans)
    }

    /// The card's unpinned chrome: title border, wrapped body, bottom border.
    fn chrome_lines(&self, width: usize) -> Vec<WizardLine> {
        let width = width.max(4);
        let inner = width - 2;
        let border = "─".repeat(inner);
        let mut lines = Vec::new();
        lines.push(wizard_title_border(&self.title, width, self.accent, true));
        for fragment in self.wrapped_body(width) {
            lines.push(self.boxed_fragment(width, &fragment));
        }
        lines.push(wizard_paint(
            &format!("└{border}┘"),
            Some(self.accent),
            false,
        ));
        lines
    }

    /// The card re-rendered to exactly `claimed_rows` physical rows, chrome
    /// pinned: title on the top border, controls on the row above the bottom
    /// border, the body windowed between them with a count when it overflows.
    fn lines_for_claim(&self, width: usize, claimed_rows: usize) -> Vec<WizardLine> {
        let width = width.max(1);
        if claimed_rows == 0 {
            return Vec::new();
        }
        let chrome = self.chrome_lines(width);
        if chrome.len() <= claimed_rows {
            let mut lines = chrome;
            while lines.len() < claimed_rows {
                lines.push(WizardLine::blank());
            }
            return lines;
        }
        // Pinned rebuild: 1 top border + body window + 1 bottom border. The
        // controls row is the last fragment; the window shows the head of the
        // body and says how much it clipped, so the way out stays reachable.
        let width = width.max(4);
        let inner = width - 2;
        let top_row = wizard_title_border(&self.title, width, self.accent, true);
        let bottom_row = wizard_paint(
            &format!("└{}┘", "─".repeat(inner)),
            Some(self.accent),
            false,
        );
        let mut body: Vec<WizardLine> = self.body.clone();
        if let Some(controls) = self.controls_line() {
            body.push(controls);
        }
        let mut fragments: Vec<WizardLine> = Vec::new();
        for line in body.iter().take(body.len().saturating_sub(1)) {
            fragments.extend(wizard_wrap(line, inner.saturating_sub(2)));
        }
        let controls_fragment = body
            .last()
            .map(|controls| wizard_wrap(controls, inner.saturating_sub(2)))
            .unwrap_or_default();

        let window = claimed_rows.saturating_sub(2);
        let mut lines = vec![top_row];
        if window == 0 {
            lines.push(bottom_row);
            return lines;
        }
        let clipped = fragments.len().saturating_sub(window.saturating_sub(1));
        let visible_head = if clipped > 0 {
            fragments[..window.saturating_sub(1)].to_vec()
        } else {
            fragments
        };
        for fragment in &visible_head {
            lines.push(self.boxed_fragment(width, fragment));
        }
        if clipped > 0 {
            lines.push(self.boxed_fragment(
                width,
                &wizard_plain(&format!("… {clipped} more lines — grow the terminal")),
            ));
        }
        for fragment in &controls_fragment {
            lines.push(self.boxed_fragment(width, fragment));
        }
        while lines.len() < claimed_rows.saturating_sub(1) {
            lines.push(WizardLine::blank());
        }
        lines.push(bottom_row);
        lines
    }
}

// ─── The view: everything one wizard frame needs ─────────────────────────────

/// Keys the wizard tree's claimable regions. Distinct from the conversation
/// `frame_key` values so a debugging layout dump never confuses the two roots.
pub mod wizard_keys {
    pub const TAB_ROW: u16 = 20;
    pub const SECTION: u16 = 21;
    pub const CARD: u16 = 22;
    pub const HELP: u16 = 23;
}

/// The section content: full logical lines plus how the host should window
/// them into the claimed leftover.
#[derive(Default)]
pub struct WizardSectionContent {
    pub lines: Vec<WizardLine>,
    /// Wrapped rows to skip from the top (the expanded GUI-status scroll).
    pub scroll_rows: usize,
    /// Line range that must stay visible (the selected feature group). The
    /// host scrolls the minimum needed to keep it inside the claim, the same
    /// guarantee ratatui's `ListState::with_selected` gave the old painter.
    pub pin_visible: Option<(usize, usize)>,
}

impl WizardSectionContent {
    /// Top-anchored styled content.
    pub fn plain(lines: Vec<WizardLine>) -> Self {
        Self {
            lines,
            scroll_rows: 0,
            pin_visible: None,
        }
    }
}

/// Everything one wizard frame paints. This is the view a GUI host (#808)
/// would consume: styled spans, tab titles with an active marker, and one
/// overlay card — the speakable canonical form without any terminal bytes.
pub struct WizardView {
    /// Header title, e.g. ` Finch Setup `.
    pub title: String,
    /// Tab names in order; `selected_tab` indexes into it. The index IS the
    /// active-tab marker prop: the tab row paints the selected tab in the
    /// active style and every other tab plainly (#1140).
    pub tab_titles: Vec<String>,
    pub selected_tab: usize,
    pub section: WizardSectionContent,
    /// One help line under the section. Yielded while a card owns the keys,
    /// exactly as the conversation composer yields behind a dialog card.
    pub help: Option<WizardLine>,
    pub card: Option<WizardCard>,
}

fn tab_row_lines(view: &WizardView, width: usize) -> Vec<String> {
    let width = width.max(4);
    let inner = width - 2;
    let top = wizard_title_border(&view.title, width, WizardColor::Blue, true);
    let mut tabs: Vec<WizardSpan> = Vec::new();
    for (index, name) in view.tab_titles.iter().enumerate() {
        if index > 0 {
            tabs.push(WizardSpan::plain("  "));
        }
        // The active tab is bold magenta ON BLACK — visible on any terminal
        // background (#1140); inactive tabs stay blue.
        let painted = if index == view.selected_tab {
            WizardSpan::with_background(name.clone(), WizardColor::Magenta, WizardColor::Black)
        } else {
            WizardSpan::styled(name.clone(), Some(WizardColor::Blue), false)
        };
        tabs.push(painted);
    }
    let used: usize = tabs.iter().map(wizard_span_visible_length).sum();
    let mut row_spans = vec![WizardSpan::styled("│", Some(WizardColor::Blue), false)];
    row_spans.extend(tabs);
    row_spans.push(WizardSpan::plain(" ".repeat(inner.saturating_sub(used))));
    row_spans.push(WizardSpan::styled("│", Some(WizardColor::Blue), false));
    let tabs_row = WizardLine(row_spans);
    vec![
        lower_wizard_line(&top),
        lower_wizard_line(&tabs_row),
        lower_wizard_line(&wizard_paint(
            &format!("└{}┘", "─".repeat(inner)),
            Some(WizardColor::Blue),
            false,
        )),
    ]
}

/// Project the wizard view into the standard claiming tree: a column whose
/// tab row and help claim their natural extent, the section claims the
/// leftover, and an open card claims its natural extent as an inline
/// [`Widget::DialogCard`] (#807) — the help yields while the card owns keys.
/// `card_lines` are the card's chrome lines at the frame width; they decide
/// the card's natural claim. All lines are lowered here: the claiming tree
/// and the paint read the same bytes.
fn project_wizard_root(view: &WizardView, width: usize, card_lines: Option<Vec<String>>) -> Widget {
    let lowered_section: Vec<String> = view.section.lines.iter().map(lower_wizard_line).collect();
    let mut children: Vec<(Track, Widget)> = vec![
        (
            Track::Natural,
            Widget::Marked(
                wizard_keys::TAB_ROW,
                Box::new(Widget::Text {
                    lines: tab_row_lines(view, width),
                }),
            ),
        ),
        (
            Track::Flex { weight: 1, min: 1 },
            Widget::Marked(
                wizard_keys::SECTION,
                Box::new(Widget::Text {
                    lines: lowered_section,
                }),
            ),
        ),
    ];
    if let Some(lines) = card_lines {
        children.push((
            Track::Natural,
            Widget::Marked(wizard_keys::CARD, Box::new(Widget::DialogCard { lines })),
        ));
    } else if let Some(help) = &view.help {
        // The help line wraps to the frame width: it names every recovery key
        // (#812 accessibility contract), so it must never overflow the frame
        // (which scrolls the whole screen) nor be truncated. Its natural
        // claim is its wrapped extent, the same lines the paint emits.
        let wrapped: Vec<String> = wizard_wrap(help, width)
            .iter()
            .map(lower_wizard_line)
            .collect();
        children.push((
            Track::Natural,
            Widget::Marked(wizard_keys::HELP, Box::new(Widget::Text { lines: wrapped })),
        ));
    }
    Widget::Stack {
        axis: Axis::Column,
        children,
    }
}

/// The claimed regions of one wizard frame, in the frame's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WizardRects {
    pub tab_row: Rect,
    pub section: Rect,
    /// The open overlay card (#807). Empty when no card is open or it was
    /// squeezed out of a tiny frame.
    pub card: Rect,
    pub help: Rect,
}

/// One planned wizard frame: the painted lines, the rects the tree claimed,
/// and each line's physical-row span for the row-diff blit.
pub struct WizardFrame {
    pub lines: Vec<String>,
    /// The regions the claiming pass gave each part of the screen. Read by
    /// the claiming tests; the blit itself paints from `lines`/`row_spans`.
    #[allow(dead_code)]
    pub rects: WizardRects,
    /// `(first physical row, physical rows)` per logical line.
    pub row_spans: Vec<(usize, usize)>,
}

impl WizardFrame {
    /// Physical rows the frame occupies once clipped to `height`.
    fn clipped_row_count(&self, height: usize) -> usize {
        self.row_spans
            .last()
            .map(|(start, rows)| (*start + *rows).min(height))
            .unwrap_or(0)
    }

    /// Render this frame into a shadow buffer of the given size, so a test can
    /// assert on the cells a terminal would end up holding. This is the
    /// blit's own record — the same buffer `WizardHost::paint` diffs against.
    #[allow(dead_code)]
    pub fn to_shadow_buffer(&self, width: usize, height: usize) -> ShadowBuffer {
        let mut buffer = ShadowBuffer::new(width.max(1), height);
        buffer.render_lines(&self.lines);
        buffer
    }
}

/// The section lines windowed into `budget` wrapped rows: skip the view's
/// scroll offset, then honour `pin_visible` with the minimum scroll that keeps
/// the pinned range visible.
fn section_window(content: &WizardSectionContent, width: usize, budget: usize) -> Vec<WizardLine> {
    if budget == 0 || content.lines.is_empty() {
        return Vec::new();
    }
    let rows: Vec<usize> = content
        .lines
        .iter()
        .map(|line| line.physical_rows(width))
        .collect();

    // Range of line indices currently visible starting at `start`.
    let fits = |start: usize| -> (usize, usize) {
        let mut used = 0usize;
        let mut end = start;
        while end < content.lines.len() && used + rows[end] <= budget {
            used += rows[end];
            end += 1;
        }
        (start, end)
    };

    // Start from the requested scroll, clamped so some content shows.
    let mut start = content
        .scroll_rows
        .min(content.lines.len().saturating_sub(1));
    let mut window = fits(start);

    // Minimal additional scroll so the pinned range is fully visible.
    if let Some((pin_start, pin_end)) = content.pin_visible {
        let pin_rows: usize = rows[pin_start.min(rows.len())..pin_end.min(rows.len())]
            .iter()
            .sum();
        let pin_end = pin_end.min(content.lines.len());
        let pin_outside = |window: (usize, usize)| pin_start < window.0 || pin_end > window.1;
        let mut guard = 0usize;
        while pin_outside(window) && guard <= content.lines.len() {
            guard += 1;
            if pin_end > window.1 {
                // Pinned range clipped at the bottom: advance until it fits.
                start += 1;
            } else {
                // Pinned range scrolled off the top: rewind.
                start = start.saturating_sub(1);
            }
            window = fits(start);
            if pin_rows > budget {
                break; // More than a screen: show the head and stop.
            }
        }
        start = start.min(content.lines.len().saturating_sub(1));
        window = fits(start);
    }
    let (start, end) = window;
    content.lines[start..end.max(start)].to_vec()
}

/// Plan one wizard frame: run the claiming pass, then size the section and
/// card contents to the claimed rects and emit the lines in paint order.
///
/// The section is the `Flex` leftover, so its claim is content-independent;
/// the card is claimed at its natural extent and re-rendered pinned to the
/// claim. Painting reads the same rects, so what the tree claimed is exactly
/// what the frame paints.
pub fn plan_wizard_frame(view: &WizardView, width: usize, height: usize) -> WizardFrame {
    let width = width.max(1);
    let frame = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    // Claiming pass: the card claims its natural (unpinned) chrome extent.
    // The card chrome lowers here: the claim and the paint read the same
    // bytes.
    let card_natural_lines = view.card.as_ref().map(|card| {
        card.chrome_lines(width)
            .iter()
            .map(lower_wizard_line)
            .collect()
    });
    let layout = widgets::layout(&project_wizard_root(view, width, card_natural_lines), frame);
    let rects = WizardRects {
        tab_row: layout.keyed(wizard_keys::TAB_ROW).unwrap_or_default(),
        section: layout.keyed(wizard_keys::SECTION).unwrap_or_default(),
        card: layout.keyed(wizard_keys::CARD).unwrap_or_default(),
        help: layout.keyed(wizard_keys::HELP).unwrap_or_default(),
    };

    let mut lines = Vec::new();
    let mut row_spans = Vec::new();
    let mut row = 0usize;
    let push = |lines: &mut Vec<String>,
                row_spans: &mut Vec<(usize, usize)>,
                row: &mut usize,
                add: Vec<String>| {
        for line in add {
            let rows = wizard_physical_rows(&line, width).min(height.saturating_sub(*row));
            if *row >= height || rows == 0 {
                continue;
            }
            row_spans.push((*row, rows));
            lines.push(line);
            *row += rows;
        }
    };

    // Tab row: clip to the claimed rows on tiny frames.
    {
        let tab_lines = tab_row_lines(view, width);
        let claim = rects.tab_row.height.min(tab_lines.len());
        push(
            &mut lines,
            &mut row_spans,
            &mut row,
            tab_lines[..claim].to_vec(),
        );
    }

    // Section: the leftover rows, windowed and padded to exactly the claim
    // (by physical rows, the unit the claim and the blit count), so the help
    // line and any card sit on the rows the pass handed them (the same
    // pinned-box discipline the conversation card follows).
    if rects.section.height > 0 {
        let mut window = section_window(&view.section, width, rects.section.height);
        let used: usize = window.iter().map(|line| line.physical_rows(width)).sum();
        for _ in used..rects.section.height {
            window.push(WizardLine::blank());
        }
        let lowered: Vec<String> = window.iter().map(lower_wizard_line).collect();
        push(&mut lines, &mut row_spans, &mut row, lowered);
    }

    // Card: pinned to the claimed box; help yielded while it owns the keys.
    if let Some(card) = &view.card {
        if rects.card.height > 0 {
            let card_lines: Vec<String> = card
                .lines_for_claim(width, rects.card.height)
                .iter()
                .map(lower_wizard_line)
                .collect();
            push(&mut lines, &mut row_spans, &mut row, card_lines);
        }
    } else if rects.help.height > 0 {
        if let Some(help) = &view.help {
            // The same wrapped lines the claim was sized from, so the paint
            // and the claiming pass see one help block.
            let wrapped: Vec<String> = wizard_wrap(help, width)
                .iter()
                .map(lower_wizard_line)
                .collect();
            push(&mut lines, &mut row_spans, &mut row, wrapped);
        }
    }

    WizardFrame {
        lines,
        rects,
        row_spans,
    }
}

// ─── The host: shadow-buffer blit ────────────────────────────────────────────

/// The wizard's renderer host. Owns the previous frame's shadow buffer and
/// blits by row diff: only logical lines whose visible rows changed are
/// rewritten, and the buffer is the authority on what a reader sees.
#[derive(Default)]
pub struct WizardHost {
    previous: Option<ShadowBuffer>,
}

impl WizardHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Blit one frame to `out`.
    ///
    /// The frame is rendered into a fresh shadow buffer; the previous buffer's
    /// rows decide which logical lines are repainted. A size change or a first
    /// frame falls back to a full clear-and-paint. The cursor is hidden: the
    /// wizard paints its own block cursors inside text fields.
    pub fn paint(
        &mut self,
        out: &mut impl Write,
        frame: &WizardFrame,
        width: usize,
        height: usize,
    ) -> Result<()> {
        let width = width.max(1);
        let height = height.max(1);
        let mut buffer = ShadowBuffer::new(width, height);
        buffer.render_lines(&frame.lines);
        let previous = self.previous.replace(buffer.clone_buffer());

        execute!(out, Hide)?;
        execute!(out, BeginSynchronizedUpdate)?;

        let repaint_all = match &previous {
            Some(previous) => previous.width != width || previous.height != height,
            None => true,
        };
        if repaint_all {
            execute!(out, Clear(ClearType::All))?;
            execute!(out, crossterm::cursor::MoveTo(0, 0))?;
            for (index, line) in frame.lines.iter().enumerate() {
                if index > 0 {
                    execute!(out, Print("\r\n"))?;
                }
                execute!(out, Print(line))?;
            }
        } else {
            let previous = previous.expect("checked above");
            let rows_before = previous.rows_as_text();
            let rows_after = buffer.rows_as_text();
            for (line, (start, rows)) in frame.lines.iter().zip(&frame.row_spans) {
                let start = *start;
                let rows = *rows;
                if start >= height {
                    break;
                }
                let rows = rows.min(height - start);
                let before = rows_before.get(start..start + rows).unwrap_or(&[]);
                let after = rows_after.get(start..start + rows).unwrap_or(&[]);
                if before == after {
                    continue;
                }
                execute!(out, crossterm::cursor::MoveTo(0, start as u16))?;
                execute!(out, Print(line))?;
                execute!(out, Clear(ClearType::UntilNewLine))?;
            }
            // Rows the shrunken frame no longer covers: clear the stale ones.
            let covered = frame.clipped_row_count(height);
            for row in covered..height {
                if rows_before
                    .get(row)
                    .is_some_and(|before| !before.is_empty())
                {
                    execute!(out, crossterm::cursor::MoveTo(0, row as u16))?;
                    execute!(out, Clear(ClearType::UntilNewLine))?;
                }
            }
        }

        execute!(out, EndSynchronizedUpdate)?;
        out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_view(section_lines: Vec<&str>) -> WizardView {
        WizardView {
            title: " Finch Setup ".to_string(),
            tab_titles: vec!["Alpha ✓".to_string(), "Beta".to_string()],
            selected_tab: 1,
            section: WizardSectionContent::plain(
                section_lines.into_iter().map(WizardLine::plain).collect(),
            ),
            help: Some(WizardLine::plain("help line")),
            card: None,
        }
    }

    #[test]
    fn test_wizard_tree_claims_tab_row_section_and_help_from_the_leftover() {
        // INVARIANT (#812/#805): the wizard frame is claimed by the same
        // widget tree the conversation uses — the 3-row tab block and the
        // 1-row help claim natural extents, the section claims the leftover,
        // and the rects drive the paint.
        let view = plain_view(vec!["provider one", "provider two"]);
        let frame = plan_wizard_frame(&view, 80, 24);
        assert_eq!(
            (frame.rects.tab_row.y, frame.rects.tab_row.height),
            (0, 3),
            "the tab block claims the top three rows; got {rect:?}",
            rect = frame.rects.tab_row
        );
        assert_eq!(
            (frame.rects.section.y, frame.rects.section.height),
            (3, 20),
            "the section claims the leftover between tab block and help; got {rect:?}",
            rect = frame.rects.section
        );
        assert_eq!(
            (frame.rects.help.y, frame.rects.help.height),
            (23, 1),
            "the help line claims the bottom row; got {rect:?}",
            rect = frame.rects.help
        );
        let rows = frame.to_shadow_buffer(80, 24).rows_as_text();
        assert!(
            rows.iter().any(|row| row.contains("provider one")),
            "the section text must land in the shadow buffer; rows={rows:?}"
        );
        assert!(
            rows[23].contains("help line"),
            "the help line must sit on the claimed bottom row; rows={rows:?}"
        );
    }

    #[test]
    fn test_resize_reclaims_every_wizard_rect_without_stale_sizes() {
        // INVARIANT (#805): resize is another claiming pass; no widget keeps
        // a cell count from the previous frame.
        let view = plain_view(vec!["content"]);
        let narrow = plan_wizard_frame(&view, 40, 18);
        let wide = plan_wizard_frame(&view, 120, 40);
        assert_eq!(narrow.rects.section.width, 40, "narrow section width");
        assert_eq!(wide.rects.section.width, 120, "wide section width");
        assert_eq!(
            narrow.rects.section.height,
            14,
            "18 rows minus 3 tab rows and 1 help row; got {rect:?}",
            rect = narrow.rects.section
        );
        assert_eq!(
            wide.rects.section.height,
            36,
            "40 rows minus 3 tab rows and 1 help row; got {rect:?}",
            rect = wide.rects.section
        );
    }

    #[test]
    fn test_overlay_card_claims_a_rect_whose_chrome_stays_inside_it() {
        // INVARIANT (#812/#807): the device-code overlay is a claimed card —
        // its title and controls are painted inside the card's own rect, and
        // the card claims exactly the rows its content needs.
        let card = WizardCard::new(
            "Device sign-in",
            vec![
                WizardLine::plain("Open: https://example/activate"),
                WizardLine::blank(),
            ],
            Some(WizardLine::plain("Esc: Cancel")),
        );
        let view = WizardView {
            card: Some(card),
            ..plain_view(vec!["section content"])
        };
        let frame = plan_wizard_frame(&view, 80, 24);
        let card_rect = frame.rects.card;
        assert!(
            card_rect.height >= 5,
            "the card must claim its content's rows; got {card_rect:?}"
        );
        assert_eq!(
            card_rect.width, 80,
            "the card claims the column's full width like a #807 card"
        );
        let rows = frame.to_shadow_buffer(80, 24).rows_as_text();
        let top = &rows[card_rect.y];
        let bottom = &rows[card_rect.y + card_rect.height - 1];
        assert!(
            top.starts_with('┌') && top.contains("Device sign-in"),
            "the title must be chrome ON the top border inside the card; top={top:?}"
        );
        assert!(
            bottom.starts_with('└') && bottom.ends_with('┘'),
            "the bottom border must close inside the claimed rect; bottom={bottom:?}"
        );
        let controls_row = &rows[card_rect.y + card_rect.height - 2];
        assert!(
            controls_row.contains("Esc: Cancel"),
            "the controls row is pinned above the bottom border, inside the card; got {controls_row:?}"
        );
        for row in &rows[card_rect.y..card_rect.y + card_rect.height] {
            assert!(
                row.starts_with('┌')
                    || row.starts_with('│')
                    || row.starts_with('└')
                    || row.trim().is_empty(),
                "no chrome may escape the card: {row:?}"
            );
        }
        assert!(
            frame.rects.help.is_empty(),
            "the help line must yield while the card owns the keys"
        );
    }

    #[test]
    fn test_card_squeezes_to_a_small_claim_with_controls_pinned() {
        // A tiny frame must never push Yes/No-style controls off the card
        // (#435 discipline applied to the wizard's cards).
        let card = WizardCard::new(
            "Add Cloud Provider",
            (0..30)
                .map(|i| WizardLine::plain(format!("body line {i}")))
                .collect(),
            Some(WizardLine::plain("Enter: adds · Esc: back")),
        );
        let view = WizardView {
            card: Some(card),
            ..plain_view(Vec::new())
        };
        let frame = plan_wizard_frame(&view, 60, 12);
        let claimed = frame.rects.card.height;
        assert!(
            claimed > 0 && claimed <= 12,
            "the card claims what the frame can afford; got {claimed}"
        );
        let card_lines = &frame.lines;
        let rendered = card_lines.join("\n");
        assert!(
            rendered.contains("Esc: back"),
            "controls stay inside a squeezed card; card frame: {rendered}"
        );
        assert!(
            rendered.contains("more lines"),
            "an overflowing body says what it clipped; card frame:\n{rendered}"
        );
    }

    #[test]
    fn test_help_yields_and_returns_as_the_card_opens_and_closes() {
        let view_open = WizardView {
            card: Some(WizardCard::new(
                "Cancel setup?",
                vec![],
                Some(WizardLine::plain("N")),
            )),
            ..plain_view(vec!["content"])
        };
        let frame_open = plan_wizard_frame(&view_open, 80, 24);
        assert!(
            frame_open.rects.help.is_empty() && frame_open.rects.card.height > 0,
            "card open: help yields, card claims"
        );
        let frame_closed = plan_wizard_frame(&plain_view(vec!["content"]), 80, 24);
        assert_eq!(
            (
                frame_closed.rects.help.height,
                frame_closed.rects.card.height
            ),
            (1, 0),
            "card closed: the help line returns and no card is claimed"
        );
    }

    #[test]
    fn test_section_window_keeps_the_pinned_range_visible() {
        // The ListState guarantee from the old painter: the selected group
        // stays visible with the minimum scroll.
        let lines: Vec<WizardLine> = (0..10)
            .map(|i| WizardLine::plain(format!("row {i:02}")))
            .collect();
        let content = WizardSectionContent {
            lines,
            scroll_rows: 0,
            pin_visible: Some((8, 10)),
        };
        let window = section_window(&content, 80, 4);
        let texts: Vec<String> = window.iter().map(WizardLine::plain_text).collect();
        assert_eq!(
            texts,
            vec![
                "row 06".to_string(),
                "row 07".to_string(),
                "row 08".to_string(),
                "row 09".to_string()
            ],
            "the window must slide so the pinned range is the last thing visible; got {texts:?}"
        );
        let pinned = WizardSectionContent {
            lines: (0..10)
                .map(|i| WizardLine::plain(format!("row {i:02}")))
                .collect(),
            scroll_rows: 0,
            pin_visible: Some((0, 2)),
        };
        let window = section_window(&pinned, 80, 4);
        assert_eq!(
            window.first().map(|line| line.plain_text()),
            Some("row 00".to_string())
        );
    }

    #[test]
    fn test_host_blits_only_changed_rows_through_the_shadow_buffer() {
        // The blit contract: the shadow buffer decides what a reader sees;
        // only lines whose visible rows changed are rewritten.
        let mut sink: Vec<u8> = Vec::new();
        let frame_a = plan_wizard_frame(&plain_view(vec!["A one", "A two"]), 40, 8);
        let frame_b = plan_wizard_frame(&plain_view(vec!["A one", "CHANGED two"]), 40, 8);

        let mut host = WizardHost::new();
        host.paint(&mut sink, &frame_a, 40, 8).unwrap();
        let after_first = sink.len();
        assert!(
            String::from_utf8_lossy(&sink).contains("A one"),
            "the first blit must paint the frame: {}",
            String::from_utf8_lossy(&sink)
        );

        host.paint(&mut sink, &frame_a, 40, 8).unwrap();
        // An identical frame rewrites no rows: the delta is terminal-mode
        // control bytes only (cursor hide, sync bracket), no printed text.
        let identical = String::from_utf8_lossy(&sink[after_first..]).to_string();
        assert!(
            !identical.contains("A one") && !identical.contains("A two"),
            "an identical frame must not reprint rows; delta was: {identical}"
        );

        let before_second = sink.len();
        host.paint(&mut sink, &frame_b, 40, 8).unwrap();
        let diff = String::from_utf8_lossy(&sink[before_second..]).to_string();
        assert!(
            diff.contains("CHANGED two"),
            "the changed line is rewritten; diff was: {diff}"
        );
        assert!(
            !diff.contains("A one"),
            "the unchanged line must not be repainted; diff was: {diff}"
        );
        let after = frame_b.to_shadow_buffer(40, 8).rows_as_text();
        assert!(
            after.iter().any(|row| row.contains("CHANGED two")),
            "the shadow buffer holds the new frame's visible rows; got {after:?}"
        );
    }

    #[test]
    fn test_host_full_repaint_on_resize() {
        let view = plain_view(vec!["content"]);
        let frame = plan_wizard_frame(&view, 40, 8);
        let mut host = WizardHost::new();
        let mut sink: Vec<u8> = Vec::new();
        host.paint(&mut sink, &frame, 40, 8).unwrap();
        let after_first = sink.len();
        // A different size is a geometry change: full clear + repaint.
        host.paint(&mut sink, &frame, 60, 10).unwrap();
        let second = &sink[after_first..];
        assert!(
            String::from_utf8_lossy(second).contains("content"),
            "a resize repaints the frame; got {}",
            String::from_utf8_lossy(second)
        );
    }

    #[test]
    fn test_frame_rects_never_escape_the_frame() {
        // Hostile sizes: nothing may claim outside the offered box.
        let card = WizardCard::new(
            "T",
            (0..40)
                .map(|i| WizardLine::plain(format!("line {i}")))
                .collect(),
            None,
        );
        for (width, height) in [(1, 1), (2, 2), (10, 3), (80, 3), (20, 1)] {
            let view = WizardView {
                card: Some(WizardCard::new(
                    card.title.clone(),
                    (0..30)
                        .map(|i| WizardLine::plain(format!("line {i}")))
                        .collect(),
                    Some(WizardLine::plain("Esc")),
                )),
                ..plain_view(
                    (0..20)
                        .map(|i| Box::leak(format!("section {i}").into_boxed_str()) as &str)
                        .collect(),
                )
            };
            let frame = plan_wizard_frame(&view, width, height);
            for (name, rect) in [
                ("tab_row", frame.rects.tab_row),
                ("section", frame.rects.section),
                ("card", frame.rects.card),
                ("help", frame.rects.help),
            ] {
                assert!(
                    rect.right() <= width.max(1) && rect.bottom() <= height,
                    "{name} rect escapes a {width}x{height} frame: {rect:?}"
                );
            }
            assert_eq!(
                frame.row_spans.len(),
                frame.lines.len(),
                "every painted line carries a row span for the blit diff"
            );
        }
    }

    // ─── #926 regressions: the blit must agree with a real terminal ──────────

    /// A minimal xterm-class terminal: exactly the sequences `WizardHost::paint`
    /// emits — cursor addressing, erase in display/line, deferred autowrap,
    /// linefeed scroll — plus the double-width emoji cells a real terminal
    /// renders. Synchronized-update brackets and SGR are consumed as noise.
    #[derive(Default)]
    struct BlitVt {
        width: usize,
        height: usize,
        screen: Vec<Vec<char>>,
        scrolled: usize,
        row: usize,
        col: usize,
        pending_wrap: bool,
    }

    impl BlitVt {
        fn new(width: usize, height: usize) -> Self {
            Self {
                width,
                height,
                screen: vec![vec![' '; width]; height],
                scrolled: 0,
                row: 0,
                col: 0,
                pending_wrap: false,
            }
        }

        /// Replay a whole byte stream.
        fn feed_all(width: usize, height: usize, bytes: &[u8]) -> Self {
            let mut vt = Self::new(width, height);
            vt.feed(&String::from_utf8_lossy(bytes));
            vt
        }

        fn feed(&mut self, text: &str) {
            let chars: Vec<char> = text.chars().collect();
            let mut index = 0;
            while index < chars.len() {
                index = self.step(&chars, index);
            }
        }

        fn step(&mut self, chars: &[char], index: usize) -> usize {
            match chars[index] {
                '\x1b' => self.escape(chars, index + 1),
                '\r' => {
                    self.col = 0;
                    self.pending_wrap = false;
                    index + 1
                }
                '\n' => {
                    self.line_feed();
                    index + 1
                }
                c if (c as u32) < 0x20 => index + 1,
                c => {
                    self.put(c);
                    index + 1
                }
            }
        }

        fn escape(&mut self, chars: &[char], index: usize) -> usize {
            match chars.get(index) {
                Some('[') => {
                    let mut cursor = index + 1;
                    let start = cursor;
                    while cursor < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[cursor]) {
                        cursor += 1;
                    }
                    if cursor >= chars.len() {
                        return chars.len();
                    }
                    let body: String = chars[start..cursor].iter().collect();
                    self.csi(&body, chars[cursor]);
                    cursor + 1
                }
                Some(']') => {
                    let mut cursor = index + 1;
                    while cursor < chars.len() {
                        if chars[cursor] == '\u{7}'
                            || (chars[cursor] == '\x1b' && chars.get(cursor + 1) == Some(&'\\'))
                        {
                            return cursor + 2;
                        }
                        cursor += 1;
                    }
                    chars.len()
                }
                Some(_) => index + 1,
                None => index,
            }
        }

        fn csi(&mut self, body: &str, final_byte: char) {
            if body.starts_with('?') || body.starts_with('>') {
                return; // mode sets, synchronized updates, mouse: screen noise
            }
            let params: Vec<usize> = body
                .split(';')
                .map(|part| part.parse::<usize>().unwrap_or(0))
                .collect();
            let first = params.first().copied().unwrap_or(0);
            let count = first.max(1);
            match final_byte {
                'H' | 'f' => {
                    self.row = first.saturating_sub(1).min(self.height - 1);
                    self.col = params
                        .get(1)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1)
                        .min(self.width - 1);
                    self.pending_wrap = false;
                }
                'J' => {
                    if first >= 2 {
                        self.screen = vec![vec![' '; self.width]; self.height];
                    } else if first == 0 {
                        for column in self.col..self.width {
                            self.screen[self.row][column] = ' ';
                        }
                        for row in (self.row + 1)..self.height {
                            self.screen[row] = vec![' '; self.width];
                        }
                    }
                }
                'K' => {
                    match first {
                        0 => {
                            for column in self.col..self.width {
                                self.screen[self.row][column] = ' ';
                            }
                        }
                        2 => self.screen[self.row] = vec![' '; self.width],
                        _ => {}
                    }
                    self.pending_wrap = false;
                }
                'A' => self.row = self.row.saturating_sub(count),
                'B' => self.row = (self.row + count).min(self.height - 1),
                'C' => self.col = (self.col + count).min(self.width - 1),
                'D' => self.col = self.col.saturating_sub(count),
                _ => {}
            }
        }

        fn line_feed(&mut self) {
            self.pending_wrap = false;
            if self.row + 1 < self.height {
                self.row += 1;
            } else {
                self.screen.remove(0);
                self.screen.push(vec![' '; self.width]);
                self.scrolled += 1;
            }
        }

        fn put(&mut self, c: char) {
            let wide = wizard_char_width(c) == 2;
            if self.pending_wrap {
                self.pending_wrap = false;
                self.col = 0;
                self.line_feed();
            }
            if wide && self.col + 2 > self.width {
                self.col = 0;
                self.line_feed();
            }
            if self.row < self.height && self.col < self.width {
                self.screen[self.row][self.col] = c;
            }
            self.col += 1;
            if wide {
                if self.col < self.width && self.row < self.height {
                    self.screen[self.row][self.col] = ' ';
                }
                self.col += 1;
            }
            if self.col >= self.width {
                self.pending_wrap = true;
            }
        }

        /// The visible screen, trailing blanks trimmed per row.
        fn rows(&self) -> Vec<String> {
            self.screen
                .iter()
                .map(|row| row.iter().collect::<String>().trim_end().to_string())
                .collect()
        }

        /// First differing row between two screens, with both payloads.
        fn first_difference(&self, expected: &Self) -> Option<String> {
            let mine = self.rows();
            let theirs = expected.rows();
            for row in 0..self.height.max(expected.height) {
                let actual = mine.get(row).map(String::as_str).unwrap_or("");
                let wanted = theirs.get(row).map(String::as_str).unwrap_or("");
                if actual != wanted {
                    return Some(format!(
                        "row {row}: terminal holds {actual:?}, a fresh paint of the same \
                         frame holds {wanted:?}"
                    ));
                }
            }
            None
        }
    }

    fn emoji_section_view(section_lines: Vec<WizardLine>) -> WizardView {
        WizardView {
            title: " Finch Setup ".to_string(),
            tab_titles: vec![
                "Look & Feel".to_string(),
                "Model Setup".to_string(),
                "Style".to_string(),
                "Settings".to_string(),
                "Finish".to_string(),
            ],
            selected_tab: 0,
            section: WizardSectionContent::plain(section_lines),
            help: Some(WizardLine::plain("↑/↓: Choose | Enter: Next | Tab: Next")),
            card: None,
        }
    }

    /// The border lines are wrapped in SGR spans; strip them so assertions can
    /// look at the glyphs a reader sees.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn test_host_blit_screen_matches_a_fresh_paint_and_never_scrolls() {
        // INVARIANT (#926): the row-diff blit's net effect on a terminal is a
        // fresh paint of the same frame — nothing scrolls, no row is shifted,
        // no stale row survives. The wizard's own content renders emoji
        // double-width; measurement that disagrees with the terminal makes the
        // first frame scroll and the diff then never repairs the tab row.
        let width = 100;
        let height = 24;
        let tall = vec![wizard_centered(
            wizard_bold("Theme Selection", WizardColor::Blue),
            width,
        )]
        .into_iter()
        .chain(wizard_boxed(
            "Available Themes",
            &[
                wizard_plain("🔧 Tool: Reading file..."),
                wizard_plain("❌ Error: File not found"),
                wizard_plain("Solarized - Solarized Dark color palette"),
                wizard_plain(
                    "A much longer description line that must wrap inside the box \
                                  because it exceeds the interior width of the box by far",
                ),
            ],
            WizardColor::Blue,
            width,
        ))
        .collect::<Vec<_>>();
        let shorter = wizard_boxed(
            "AI Providers",
            &[wizard_plain("★ Primary: claude [Not configured]")],
            WizardColor::Blue,
            width,
        );

        let frames: Vec<WizardFrame> = [
            emoji_section_view(tall.clone()),
            emoji_section_view(tall),
            emoji_section_view(shorter.clone()),
            emoji_section_view(vec![wizard_plain("tiny")]),
        ]
        .iter()
        .map(|view| plan_wizard_frame(view, width, height))
        .collect();

        let mut host = WizardHost::new();
        let mut sink: Vec<u8> = Vec::new();
        for (step, frame) in frames.iter().enumerate() {
            host.paint(&mut sink, frame, width, height).unwrap();
            let terminal = BlitVt::feed_all(width, height, &sink);
            assert_eq!(
                terminal.scrolled,
                0,
                "frame {step} scrolled the terminal; the wizard planned rows that do \
                 not match what a terminal renders. Screen:\n{}",
                terminal.rows().join("\n")
            );
            let mut expected = BlitVt::new(width, height);
            let mut ideal = String::new();
            for (index, line) in frame.lines.iter().enumerate() {
                if index > 0 {
                    ideal.push_str("\r\n");
                }
                ideal.push_str(line);
            }
            expected.feed(&ideal);
            if let Some(difference) = terminal.first_difference(&expected) {
                panic!(
                    "INVARIANT (#926): after frame {step} the blitted screen must equal a \
                     fresh paint of the same frame; {difference}\nterminal:\n{}\nexpected:\n{}",
                    terminal.rows().join("\n"),
                    expected.rows().join("\n")
                );
            }
        }
    }

    #[test]
    fn test_wizard_title_border_is_a_glyph_run_of_exact_width() {
        // INVARIANT (#926): box/tab top borders repeat the ─ glyph to exactly
        // the frame width — never the gap count as digits, never a row that
        // overflows or falls short of the frame.
        for width in [5, 6, 10, 40, 80, 100, 120, 124, 200] {
            for title in [
                "",
                "Available Themes",
                " Finch Setup ",
                "Full GUI automation status (read/scroll only)",
            ] {
                for (accent, bold) in [(WizardColor::Blue, false), (WizardColor::Cyan, true)] {
                    let border = wizard_title_border(title, width, accent, bold);
                    let border = lower_wizard_line(&border);
                    let visible = wizard_visible_length(&border);
                    assert_eq!(
                        visible, width,
                        "border for {title:?} at width {width} must be exactly {width} columns; got {border:?}"
                    );
                    let plain = strip_ansi(&border);
                    assert!(
                        plain.starts_with('┌') && plain.ends_with('┐'),
                        "border must close its box; got {border:?}"
                    );
                    let digits = plain.chars().any(|ch| ch.is_ascii_digit());
                    assert!(
                        !digits,
                        "border for {title:?} at width {width} must not carry the gap \
                         count as digits; got {border:?}"
                    );
                }
            }
        }
        // A row too small for a title still closes without overflowing.
        for width in [1, 2, 4] {
            let border = wizard_title_border("Available Themes", width, WizardColor::Blue, false);
            assert!(
                border.display_length() <= width,
                "border at width {width} overflows the frame: {border:?}"
            );
        }
    }

    #[test]
    fn test_wizard_emoji_rows_measure_terminal_columns() {
        // INVARIANT (#926): the wizard's measurement must match what a
        // terminal renders — emoji-presentation characters occupy two columns,
        // so padded box rows stay inside the frame instead of wrapping onto
        // the next row and desyncing the row-diff blit.
        assert_eq!(wizard_char_width('🔧'), 2);
        assert_eq!(wizard_char_width('❌'), 2);
        assert_eq!(wizard_char_width('✅'), 2);
        assert_eq!(
            wizard_char_width('★'),
            1,
            "ambiguous width stays one column"
        );
        assert_eq!(wizard_visible_length("🔧 Tool: "), 9);
        let width = 100;
        let boxed = wizard_boxed(
            "Preview",
            &[
                wizard_plain("🔧 Tool: Reading file..."),
                wizard_plain("❌ Error: File not found"),
            ],
            WizardColor::Blue,
            width,
        );
        for line in &boxed {
            assert!(
                line.display_length() <= width,
                "box row must fit the frame as a terminal measures it: {line:?}"
            );
            assert_eq!(
                line.physical_rows(width),
                1,
                "box row must occupy one terminal row: {line:?}"
            );
        }
        // The old measurement counted the emoji as one column and padded the
        // row past the frame; the widest row pins the two-column accounting.
        let body = &boxed[2];
        assert_eq!(
            body.display_length(),
            width,
            "padded box rows fill the frame exactly; got {body:?}"
        );
    }

    #[test]
    fn test_tab_row_marks_the_active_tab() {
        // INVARIANT (#926): the tab row renders every label and marks the
        // active section (bold + magenta) so navigation is visible.
        let view = emoji_section_view(vec![WizardLine::plain("content")]);
        let selected = &view.tab_titles[view.selected_tab];
        let rows = tab_row_lines(&view, 100);
        let tabs = &rows[1];
        for name in &view.tab_titles {
            assert!(
                strip_ansi(tabs).contains(name.as_str()),
                "tab label {name:?} must be painted in the tab row; got {tabs:?}"
            );
        }
        // #1140: the active tab is bold magenta ON BLACK — visually distinct
        // on any terminal background, not just a bold weight.
        let marked = lower_wizard_line(&WizardLine(vec![WizardSpan::with_background(
            selected.clone(),
            WizardColor::Magenta,
            WizardColor::Black,
        )]));
        assert!(
            tabs.contains(&marked),
            "the active tab must be the bold-magenta-on-black span {marked:?}; tab row: {tabs:?}"
        );
        let inactive = view
            .tab_titles
            .iter()
            .enumerate()
            .find(|(index, _)| *index != view.selected_tab)
            .map(|(_, name)| lower_wizard_line(&wizard_line(name, WizardColor::Blue)))
            .unwrap();
        assert!(
            tabs.contains(&inactive),
            "inactive tabs must not carry the active marking; tab row: {tabs:?}"
        );
        assert!(
            tabs.contains("\x1b[1;35;40m"),
            "the active tab's SGR carries bold + magenta + black background; got {tabs:?}"
        );
        assert!(
            !tabs.contains("\x1b[1;35;40m\u{1b}[0m\u{1b}[1;35;40m"),
            "exactly one tab wears the active marking; got {tabs:?}"
        );
    }
}
