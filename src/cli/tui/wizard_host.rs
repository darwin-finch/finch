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

use super::shadow_buffer::{self, ShadowBuffer};
use super::widgets::{self, Axis, Rect, Track, Widget};
use anyhow::Result;
use crossterm::{
    cursor::Hide,
    execute,
    style::Print,
    terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};

// ─── Styling ─────────────────────────────────────────────────────────────────
//
// Wizard lines carry their own ANSI, exactly like conversation live-frame
// lines. The shadow buffer strips the codes for measurement, so styling can
// never change a frame's geometry.

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

/// SGR reset closing every styled wizard span.
const WIZ_RESET: &str = "\x1b[0m";

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

/// One styled wizard span: optional foreground colour plus bold.
pub fn wizard_paint(text: &str, fg: Option<WizardColor>, bold: bool) -> String {
    let mut code = String::new();
    if bold {
        code.push('1');
    }
    if let Some(fg) = fg {
        if !code.is_empty() {
            code.push(';');
        }
        code.push_str(&sgr_fg(fg));
    }
    if code.is_empty() {
        text.to_string()
    } else {
        format!("\x1b[{code}m{text}{WIZ_RESET}")
    }
}

/// A coloured, non-bold wizard span.
pub fn wizard_line(text: &str, fg: WizardColor) -> String {
    wizard_paint(text, Some(fg), false)
}

/// A coloured, bold wizard span.
pub fn wizard_bold(text: &str, fg: WizardColor) -> String {
    wizard_paint(text, Some(fg), true)
}

/// A plain wizard span.
pub fn wizard_plain(text: &str) -> String {
    wizard_paint(text, None, false)
}

/// Centre `text` (ANSI-aware) in `width` display columns.
pub fn wizard_centered(text: &str, width: usize) -> String {
    let lead = width.saturating_sub(shadow_buffer::visible_length(text)) / 2;
    format!("{}{}", " ".repeat(lead), text)
}

/// Word-wrap `text` at `width` display columns, keeping any single leading
/// SGR span on every fragment so wrapped box rows keep their style.
///
/// A boxed body must wrap like the old painter's `Paragraph`, not truncate —
/// truncation is exactly how an accessibility string loses its last words.
pub fn wizard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    // Split a single styled span into (leading escapes, inner text, reset).
    let mut prefix = String::new();
    let mut rest = text;
    while rest.starts_with('\x1b') {
        let Some(end) = rest.find('m').or_else(|| rest.find('\x07')) else {
            break;
        };
        let (escape, remainder) = rest.split_at(end + 1);
        if escape.ends_with("[0m") {
            break; // A reset ends the leading style run.
        }
        prefix.push_str(escape);
        rest = remainder;
    }
    let inner = rest.strip_suffix(WIZ_RESET).unwrap_or(rest);

    // Greedy word wrap over display columns.
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut column = 0usize;
    for word in inner.split(' ') {
        if column > 0 && column + shadow_buffer::visible_length(word) > width {
            lines.push(format!("{prefix}{}{WIZ_RESET}", current.trim_end()));
            current.clear();
            column = 0;
        }
        for ch in word.chars() {
            let char_width = shadow_buffer::visible_length(&ch.to_string());
            if column + char_width > width {
                lines.push(format!("{prefix}{current}{WIZ_RESET}"));
                current.clear();
                column = 0;
            }
            current.push(ch);
            column += char_width;
        }
        current.push(' ');
        column += 1;
    }
    if current.trim().is_empty() {
        if lines.is_empty() {
            lines.push(String::new());
        }
    } else {
        lines.push(format!("{prefix}{}{WIZ_RESET}", current.trim_end()));
    }
    lines
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

/// One boxed region of a wizard section: `title` on the top border, every
/// body line wrapped and padded so the accent border stays on the box.
pub fn wizard_boxed(
    title: &str,
    body: &[String],
    accent: WizardColor,
    width: usize,
) -> Vec<String> {
    let width = width.max(4);
    let inner = width - 2;
    let border = "─".repeat(inner);
    let title_gap = inner.saturating_sub(title.chars().count() + 2);
    let mut out = vec![wizard_paint(
        &format!("┌─ {title} ─{title_gap}─┐"),
        Some(accent),
        false,
    )];
    for line in body {
        for fragment in wizard_wrap(line, inner.saturating_sub(2)) {
            let pad = inner
                .saturating_sub(2)
                .saturating_sub(shadow_buffer::visible_length(&fragment));
            out.push(format!(
                "{} {}{} {}",
                wizard_paint("│", Some(accent), false),
                fragment,
                " ".repeat(pad),
                wizard_paint("│", Some(accent), false)
            ));
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
    pub body: Vec<String>,
    pub controls: Option<String>,
    pub accent: WizardColor,
}

impl WizardCard {
    /// A card that announces and instructs: title, body, controls, cyan chrome.
    pub fn new(title: impl Into<String>, body: Vec<String>, controls: Option<String>) -> Self {
        Self {
            title: title.into(),
            body,
            controls,
            accent: WizardColor::Cyan,
        }
    }

    /// The body (controls pinned last) wrapped to one fragment per row, the
    /// unit the chrome and the pinned rebuild both count.
    fn wrapped_body(&self, width: usize) -> Vec<String> {
        let width = width.max(4);
        let inner = width - 2;
        let mut body: Vec<String> = self.body.clone();
        if let Some(controls) = &self.controls {
            body.push(wizard_line(controls, WizardColor::Yellow));
        }
        let mut fragments = Vec::new();
        for line in &body {
            fragments.extend(wizard_wrap(line, inner.saturating_sub(2)));
        }
        fragments
    }

    fn boxed_fragment(&self, width: usize, fragment: &str) -> String {
        let width = width.max(4);
        let inner = width - 2;
        let pad = inner
            .saturating_sub(2)
            .saturating_sub(shadow_buffer::visible_length(fragment));
        format!(
            "{} {}{} {}",
            wizard_paint("│", Some(self.accent), false),
            fragment,
            " ".repeat(pad),
            wizard_paint("│", Some(self.accent), false)
        )
    }

    /// The card's unpinned chrome: title border, wrapped body, bottom border.
    fn chrome_lines(&self, width: usize) -> Vec<String> {
        let width = width.max(4);
        let inner = width - 2;
        let border = "─".repeat(inner);
        let mut lines = Vec::new();
        let title_gap = inner.saturating_sub(self.title.chars().count() + 2);
        lines.push(wizard_bold(
            &format!("┌─ {} ─{title_gap}─┐", self.title),
            self.accent,
        ));
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
    fn lines_for_claim(&self, width: usize, claimed_rows: usize) -> Vec<String> {
        let width = width.max(1);
        if claimed_rows == 0 {
            return Vec::new();
        }
        let chrome = self.chrome_lines(width);
        if chrome.len() <= claimed_rows {
            let mut lines = chrome;
            while lines.len() < claimed_rows {
                lines.push(String::new());
            }
            return lines;
        }
        // Pinned rebuild: 1 top border + body window + 1 bottom border. The
        // controls row is the last fragment; the window shows the head of the
        // body and says how much it clipped, so the way out stays reachable.
        let width = width.max(4);
        let inner = width - 2;
        let top_row = wizard_bold(&format!("┌─ {} ─┐", self.title), self.accent);
        let bottom_row = wizard_paint(
            &format!("└{}┘", "─".repeat(inner)),
            Some(self.accent),
            false,
        );
        let mut body: Vec<String> = self.body.clone();
        if let Some(controls) = &self.controls {
            body.push(wizard_line(controls, WizardColor::Yellow));
        }
        let mut fragments: Vec<String> = Vec::new();
        for line in body.iter().take(body.len().saturating_sub(1)) {
            fragments.extend(wizard_wrap(line, inner.saturating_sub(2)));
        }
        let controls_fragment = body
            .last()
            .cloned()
            .map(|controls| wizard_wrap(&controls, inner.saturating_sub(2)))
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
            lines.push(String::new());
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
    pub lines: Vec<String>,
    /// Wrapped rows to skip from the top (the expanded GUI-status scroll).
    pub scroll_rows: usize,
    /// Line range that must stay visible (the selected feature group). The
    /// host scrolls the minimum needed to keep it inside the claim, the same
    /// guarantee ratatui's `ListState::with_selected` gave the old painter.
    pub pin_visible: Option<(usize, usize)>,
}

impl WizardSectionContent {
    /// Plain top-anchored content.
    pub fn plain(lines: Vec<String>) -> Self {
        Self {
            lines,
            scroll_rows: 0,
            pin_visible: None,
        }
    }
}

/// Everything one wizard frame paints. This is the view a GUI host (#808)
/// would consume: plain text, tab titles, and one overlay card.
pub struct WizardView {
    /// Header title, e.g. ` Finch Setup `.
    pub title: String,
    /// Tab names in order; `selected_tab` indexes into it.
    pub tab_titles: Vec<String>,
    pub selected_tab: usize,
    pub section: WizardSectionContent,
    /// One help line under the section. Yielded while a card owns the keys,
    /// exactly as the conversation composer yields behind a dialog card.
    pub help: Option<String>,
    pub card: Option<WizardCard>,
}

fn tab_row_lines(view: &WizardView, width: usize) -> Vec<String> {
    let width = width.max(4);
    let inner = width - 2;
    let title_gap = inner.saturating_sub(view.title.chars().count() + 2);
    let top = wizard_bold(
        &format!("┌─ {} ─{title_gap}─┐", view.title),
        WizardColor::Blue,
    );
    let mut tabs = String::new();
    for (index, name) in view.tab_titles.iter().enumerate() {
        if index > 0 {
            tabs.push_str("  ");
        }
        let painted = if index == view.selected_tab {
            wizard_bold(name, WizardColor::Magenta)
        } else {
            wizard_line(name, WizardColor::Blue)
        };
        tabs.push_str(&painted);
    }
    let tabs_row = format!(
        "{}{}{}",
        wizard_paint("│", Some(WizardColor::Blue), false),
        tabs,
        {
            let used = shadow_buffer::visible_length(&tabs);
            format!(
                "{}{}",
                " ".repeat(inner.saturating_sub(used)),
                wizard_paint("│", Some(WizardColor::Blue), false)
            )
        }
    );
    vec![
        top,
        tabs_row,
        wizard_paint(
            &format!("└{}┘", "─".repeat(inner)),
            Some(WizardColor::Blue),
            false,
        ),
    ]
}

/// Project the wizard view into the standard claiming tree: a column whose
/// tab row and help claim their natural extent, the section claims the
/// leftover, and an open card claims its natural extent as an inline
/// [`Widget::DialogCard`] (#807) — the help yields while the card owns keys.
/// `card_lines` are the card's chrome lines at the frame width; they decide
/// the card's natural claim.
fn project_wizard_root(view: &WizardView, width: usize, card_lines: Option<Vec<String>>) -> Widget {
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
                    lines: view.section.lines.clone(),
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
        children.push((
            Track::Natural,
            Widget::Marked(
                wizard_keys::HELP,
                Box::new(Widget::Text {
                    lines: vec![help.clone()],
                }),
            ),
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
fn section_window(content: &WizardSectionContent, width: usize, budget: usize) -> Vec<String> {
    if budget == 0 || content.lines.is_empty() {
        return Vec::new();
    }
    let rows: Vec<usize> = content
        .lines
        .iter()
        .map(|line| shadow_buffer::physical_rows(line, width))
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
    let card_natural_lines = view.card.as_ref().map(|card| card.chrome_lines(width));
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
            let rows = shadow_buffer::physical_rows(&line, width).min(height.saturating_sub(*row));
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

    // Section: the leftover rows, windowed and padded to exactly the claim,
    // so the help line and any card sit on the rows the pass handed them
    // (the same pinned-box discipline the conversation card follows).
    if rects.section.height > 0 {
        let mut window = section_window(&view.section, width, rects.section.height);
        while window.len() < rects.section.height {
            window.push(String::new());
        }
        push(&mut lines, &mut row_spans, &mut row, window);
    }

    // Card: pinned to the claimed box; help yielded while it owns the keys.
    if let Some(card) = &view.card {
        if rects.card.height > 0 {
            let card_lines = card.lines_for_claim(width, rects.card.height);
            push(&mut lines, &mut row_spans, &mut row, card_lines);
        }
    } else if rects.help.height > 0 {
        if let Some(help) = &view.help {
            push(&mut lines, &mut row_spans, &mut row, vec![help.clone()]);
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

    fn plain_view(section_lines: Vec<String>) -> WizardView {
        WizardView {
            title: " Finch Setup ".to_string(),
            tab_titles: vec!["Alpha ✓".to_string(), "Beta".to_string()],
            selected_tab: 1,
            section: WizardSectionContent::plain(section_lines),
            help: Some("help line".to_string()),
            card: None,
        }
    }

    #[test]
    fn test_wizard_tree_claims_tab_row_section_and_help_from_the_leftover() {
        // INVARIANT (#812/#805): the wizard frame is claimed by the same
        // widget tree the conversation uses — the 3-row tab block and the
        // 1-row help claim natural extents, the section claims the leftover,
        // and the rects drive the paint.
        let view = plain_view(vec!["provider one".to_string(), "provider two".to_string()]);
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
        let view = plain_view(vec!["content".to_string()]);
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
            vec!["Open: https://example/activate".to_string(), String::new()],
            Some("Esc: Cancel".to_string()),
        );
        let view = WizardView {
            card: Some(card),
            ..plain_view(vec!["section content".to_string()])
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
            (0..30).map(|i| format!("body line {i}")).collect(),
            Some("Enter: adds · Esc: back".to_string()),
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
            card: Some(WizardCard::new("Cancel setup?", vec![], Some("N".into()))),
            ..plain_view(vec!["content".to_string()])
        };
        let frame_open = plan_wizard_frame(&view_open, 80, 24);
        assert!(
            frame_open.rects.help.is_empty() && frame_open.rects.card.height > 0,
            "card open: help yields, card claims"
        );
        let frame_closed = plan_wizard_frame(&plain_view(vec!["content".to_string()]), 80, 24);
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
        let lines: Vec<String> = (0..10).map(|i| format!("row {i:02}")).collect();
        let content = WizardSectionContent {
            lines,
            scroll_rows: 0,
            pin_visible: Some((8, 10)),
        };
        let window = section_window(&content, 80, 4);
        assert_eq!(
            window,
            vec![
                "row 06".to_string(),
                "row 07".to_string(),
                "row 08".to_string(),
                "row 09".to_string()
            ],
            "the window must slide so the pinned range is the last thing visible; got {window:?}"
        );
        let pinned = WizardSectionContent {
            lines: (0..10).map(|i| format!("row {i:02}")).collect(),
            scroll_rows: 0,
            pin_visible: Some((0, 2)),
        };
        let window = section_window(&pinned, 80, 4);
        assert_eq!(window.first().map(String::as_str), Some("row 00"));
    }

    #[test]
    fn test_host_blits_only_changed_rows_through_the_shadow_buffer() {
        // The blit contract: the shadow buffer decides what a reader sees;
        // only lines whose visible rows changed are rewritten.
        let mut sink: Vec<u8> = Vec::new();
        let frame_a = plan_wizard_frame(
            &plain_view(vec!["A one".to_string(), "A two".to_string()]),
            40,
            8,
        );
        let frame_b = plan_wizard_frame(
            &plain_view(vec!["A one".to_string(), "CHANGED two".to_string()]),
            40,
            8,
        );

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
        let view = plain_view(vec!["content".to_string()]);
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
        let card = WizardCard::new("T", (0..40).map(|i| format!("line {i}")).collect(), None);
        for (width, height) in [(1, 1), (2, 2), (10, 3), (80, 3), (20, 1)] {
            let view = WizardView {
                card: Some(WizardCard::new(
                    card.title.clone(),
                    (0..30).map(|i| format!("line {i}")).collect(),
                    Some("Esc".into()),
                )),
                ..plain_view((0..20).map(|i| format!("section {i}")).collect())
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
}
