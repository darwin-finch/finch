//! Span → SGR lowering at paint, and the ColorScheme → component palette
//! bridge (stage 4 of docs/TUI_DESIGN.md, #1141).
//!
//! Components emit semantic [`Span`]s; this module is where the terminal mode
//! turns them into bytes — the only place in the conversation pipeline that
//! names SGR codes for spans. A style lowers to one escape pair per span
//! (`bold`/`dim`/fg/`bg` in one attribute run, closed by a reset), so a
//! single-span line renders exactly the byte shape the retired
//! `format()`-era helpers produced for the same style.
//!
//! The DOM mode (#808) lowers the same spans to styled elements instead; this
//! file never runs in that path.

use finch_theme::{ColorScheme, MessageBand};
use finch_ui_model::{ComponentStylePalette, RenderedTranscriptLine, Span, SpanColor, SpanStyle};

/// SGR parameter for one colour: foreground position.
pub(crate) fn fg_code(color: SpanColor) -> String {
    match color {
        SpanColor::Indexed(index) if index < 8 => format!("{}", 30 + index),
        SpanColor::Indexed(index) => format!("{}", 90 + (index - 8)),
        SpanColor::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
    }
}

/// SGR parameter for one colour: background position.
pub(crate) fn bg_code(color: SpanColor) -> String {
    match color {
        SpanColor::Indexed(index) if index < 8 => format!("{}", 40 + index),
        SpanColor::Indexed(index) => format!("{}", 100 + (index - 8)),
        SpanColor::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
    }
}

/// The SGR attribute run for one style: bold, then dim, then colours. A plain
/// style yields an empty run and the span paints as bare text.
fn style_codes(style: &SpanStyle) -> String {
    let mut codes: Vec<String> = Vec::new();
    if style.bold {
        codes.push("1".to_string());
    }
    if style.dim {
        codes.push("2".to_string());
    }
    if let Some(ref fg) = style.fg {
        codes.push(fg_code(fg.clone()));
    }
    if let Some(ref bg) = style.bg {
        codes.push(bg_code(bg.clone()));
    }
    codes.join(";")
}

const SGR_RESET: &str = "\x1b[0m";

/// Lower one span to the bytes a terminal paints: the style run, the span's
/// text, and a reset. A plain span is the text alone, so unstyled content
/// reaches the terminal exactly as the legacy paths wrote it.
pub fn lower_span(span: &Span) -> String {
    let codes = style_codes(&span.style);

    let mut out = String::new();
    if let Some(url) = &span.style.hyperlink {
        out.push_str(&format!("\x1b]8;;{}\x1b\\", url));
    }
    if !codes.is_empty() {
        out.push_str(&format!("\x1b[{codes}m"));
    }
    out.push_str(&span.text);
    if !codes.is_empty() {
        out.push_str(SGR_RESET);
    }
    if span.style.hyperlink.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
    }

    // Fallback if plain (but still check if we added links, in which case it wasn't fully plain)
    if out == span.text {
        span.text.clone()
    } else {
        out
    }
}

/// Lower a span sequence to one painted line.
pub fn lower_spans(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        out.push_str(&lower_span(span));
    }
    out
}

/// The painted bytes of one rendered transcript line: the lowered spans when
/// the line carries styling, otherwise the legacy plain text (which may
/// itself carry SGR from an unmigrated projection — that path is unchanged).
/// Measurement never runs on this string: SGR is zero-width and the plain
/// `text` is what every row count reads.
pub fn lower_rendered_line(
    line: &RenderedTranscriptLine,
    force_bg: Option<SpanColor>,
    canvas_bg: Option<SpanColor>,
) -> String {
    let has_bg = force_bg.is_some()
        || canvas_bg.is_some()
        || line.spans.iter().any(|s| s.style.bg.is_some());
    let mut rendered = if line.spans.is_empty() {
        let bg = force_bg.clone().or_else(|| canvas_bg.clone());
        if let Some(bg) = bg {
            let span = Span::styled(&line.text, SpanStyle::default().with_bg(bg));
            lower_span(&span)
        } else {
            line.text.clone()
        }
    } else {
        if force_bg.is_some() || canvas_bg.is_some() {
            let spans = line
                .spans
                .iter()
                .map(|s| {
                    let mut style = s.style.clone();
                    if let Some(bg) = &force_bg {
                        style.bg = Some(bg.clone());
                    } else if style.bg.is_none() {
                        if let Some(cbg) = &canvas_bg {
                            style.bg = Some(cbg.clone());
                        }
                    }
                    Span::styled(&s.text, style)
                })
                .collect::<Vec<_>>();
            lower_spans(&spans)
        } else {
            lower_spans(&line.spans)
        }
    };

    if has_bg {
        // Clear to the end of the line while the background color is active.
        // In ANSI terminals, \x1b[K (Clear Until New Line) erases to the right
        // margin with the currently active background color. Inserting \x1b[K
        // immediately prior to the closing \x1b[0m extends the background
        // cleanly across the full terminal row.
        const SGR_RESET: &str = "\x1b[0m";
        const EXTENDED_RESET: &str = "\x1b[K\x1b[0m";
        if rendered.ends_with(SGR_RESET) {
            let prefix_len = rendered.len() - SGR_RESET.len();
            rendered.truncate(prefix_len);
            rendered.push_str(EXTENDED_RESET);
        } else if let Some(reset_pos) = rendered.rfind(SGR_RESET) {
            rendered.insert_str(reset_pos, "\x1b[K");
        }
    }

    rendered
}

/// Map a `ColorSpec` (the ColorScheme's serializable colour) to the span
/// vocabulary's colour. The name table lives in `finch-theme`
/// (`ColorSpec::to_color`) and nowhere else, so configuration, ratatui
/// widgets, and span lowering cannot disagree about what a name means.
pub(crate) fn span_color_from_spec(spec: &finch_theme::ColorSpec) -> SpanColor {
    span_color_from_ratatui_color(spec.to_color())
}

/// Map a ratatui `Color` to a `SpanColor`.
fn span_color_from_ratatui_color(color: ratatui::style::Color) -> SpanColor {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => SpanColor::Rgb(r, g, b),
        ratatui::style::Color::Indexed(index) => SpanColor::Indexed(index),
        ratatui::style::Color::Black => SpanColor::BLACK,
        ratatui::style::Color::Red => SpanColor::DARK_RED,
        ratatui::style::Color::Green => SpanColor::DARK_GREEN,
        ratatui::style::Color::Yellow => SpanColor::DARK_YELLOW,
        ratatui::style::Color::Blue => SpanColor::DARK_BLUE,
        ratatui::style::Color::Magenta => SpanColor::DARK_MAGENTA,
        ratatui::style::Color::Cyan => SpanColor::DARK_CYAN,
        ratatui::style::Color::Gray => SpanColor::GREY,
        ratatui::style::Color::DarkGray => SpanColor::DARK_GREY,
        ratatui::style::Color::LightRed => SpanColor::RED,
        ratatui::style::Color::LightGreen => SpanColor::GREEN,
        ratatui::style::Color::LightYellow => SpanColor::YELLOW,
        ratatui::style::Color::LightBlue => SpanColor::BLUE,
        ratatui::style::Color::LightMagenta => SpanColor::MAGENTA,
        ratatui::style::Color::LightCyan => SpanColor::CYAN,
        ratatui::style::Color::White => SpanColor::WHITE,
        _ => SpanColor::BLACK,
    }
}

/// Build the component palette from the user's scheme: every role comes from
/// it, including the glyph vocabulary (⏺ takes the accent `ui.cursor`; ⎿,
/// summaries and ellipses take the muted `messages.system`). This is the one
/// place the conversation pipeline meets `ColorScheme` — component renderers
/// stay scheme-free.
pub fn component_style_palette(colors: &ColorScheme) -> ComponentStylePalette {
    let mut palette = ComponentStylePalette::default();
    palette.progress_running = SpanStyle::fg(span_color_from_spec(&colors.status.operation));
    palette.progress_complete = SpanStyle::fg(span_color_from_spec(&colors.status.download));
    palette.progress_failed = SpanStyle::fg(span_color_from_spec(&colors.messages.error));
    palette.static_info = SpanStyle::fg(span_color_from_spec(&colors.messages.system));
    palette.static_error = SpanStyle::fg(span_color_from_spec(&colors.messages.error));
    palette.static_success = palette.static_info.clone();
    palette.static_warning = SpanStyle::fg(span_color_from_spec(&colors.status.operation));
    let accent = span_color_from_spec(&colors.ui.cursor);
    let muted = span_color_from_spec(&colors.messages.system);
    palette.operation_glyph = SpanStyle::fg(accent);
    palette.operation_row_glyph = SpanStyle::fg(muted);
    palette.operation_summary = SpanStyle::fg(muted).with_dim(true);
    palette.operation_error = SpanStyle::fg(span_color_from_spec(&colors.messages.error));
    palette.live_tool_ellipsis = SpanStyle::fg(muted).with_dim(true);
    palette.user_foreground = span_color_from_spec(&colors.messages.user);
    if let Some(bg) = colors.message_band_style(MessageBand::LocalUser).bg {
        palette.user_background = span_color_from_ratatui_color(bg);
    }
    for i in 0..8 {
        if let Some(bg) = colors.message_band_style(MessageBand::Participant(i)).bg {
            palette.participant_backgrounds[i] = span_color_from_ratatui_color(bg);
        }
    }
    palette.hover_background = span_color_from_ratatui_color(colors.hover_background());
    palette
}

/// The scheme's canvas: the background every row is painted on, the default
/// text colour on it, and the scheme's rendering of the 16 ANSI colours. The
/// transcript and the live area below it (composer, rules, status) share one
/// canvas, so a theme whose background differs from the terminal profile's
/// does not split the screen in two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Canvas {
    pub bg: SpanColor,
    pub fg: SpanColor,
    /// `ColorScheme::ansi_color` for palette indices 0–15.
    ansi: [SpanColor; 16],
}

impl Canvas {
    pub fn from_scheme(colors: &ColorScheme) -> Self {
        let mut ansi = [SpanColor::BLACK; 16];
        for (index, slot) in ansi.iter_mut().enumerate() {
            *slot = span_color_from_spec(&colors.ansi_color(index as u8));
        }
        Self {
            bg: span_color_from_spec(&colors.background),
            fg: span_color_from_spec(&colors.foreground),
            ansi,
        }
    }

    /// The SGR run that selects the canvas colours.
    pub(crate) fn open(&self) -> String {
        format!("\x1b[{};{}m", fg_code(self.fg), bg_code(self.bg))
    }

    /// Paint one already-lowered row on the canvas.
    ///
    /// This is the single place colour reaches the terminal for a row, so it
    /// is where every colour is made to follow the scheme:
    ///
    /// - bare ANSI colours (text some producer styled with a fixed colour
    ///   instead of a scheme role) are mapped through the scheme;
    /// - "default colour" resets return to the canvas, not to the terminal
    ///   profile's own colours;
    /// - the canvas colours open the row and are re-asserted after every
    ///   full reset, so unstyled text and gaps take the canvas.
    ///
    /// The row is erased to the right margin in the canvas background
    /// *before* its text is written. Erasing after the text would delete the
    /// last glyph of a row that exactly fills the terminal width: the cursor
    /// then rests on that final column, and erase-to-end-of-line starts there.
    pub fn paint_row(&self, row: &str) -> String {
        const ERASE_TO_END: &str = "\x1b[K";
        let open = self.open();
        let body = self
            .retheme(row)
            .replace(SGR_RESET, &format!("{SGR_RESET}{open}"));
        format!("{open}{ERASE_TO_END}{body}{SGR_RESET}")
    }

    /// Rewrite every SGR run in `row` onto the scheme.
    fn retheme(&self, row: &str) -> String {
        let mut out = String::with_capacity(row.len());
        let mut rest = row;
        while let Some(start) = rest.find("\x1b[") {
            let after = &rest[start + 2..];
            let end = after
                .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                .filter(|&end| after[end..].starts_with('m'));
            let Some(end) = end else {
                // Not an SGR run (cursor movement, erase, OSC…): copy through.
                out.push_str(&rest[..start + 2]);
                rest = after;
                continue;
            };
            out.push_str(&rest[..start]);
            out.push_str(&format!("\x1b[{}m", self.retheme_params(&after[..end])));
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn retheme_params(&self, params: &str) -> String {
        let parts: Vec<&str> = params.split(';').collect();
        let number = |index: usize| parts.get(index).and_then(|part| part.parse::<u16>().ok());
        let mut out: Vec<String> = Vec::new();
        let mut index = 0;
        while index < parts.len() {
            let code = number(index);
            match code {
                // Extended colour: `38;2;r;g;b` is already a colour by value;
                // `38;5;n` names an ANSI colour only for n < 16.
                Some(extended @ (38 | 48)) => {
                    let palette = match (number(index + 1), number(index + 2)) {
                        (Some(5), Some(n)) if n < 16 => Some(self.ansi[n as usize]),
                        _ => None,
                    };
                    let width = match number(index + 1) {
                        Some(2) => 5,
                        Some(5) => 3,
                        _ => 1,
                    };
                    match palette {
                        Some(color) if extended == 38 => out.push(fg_code(color)),
                        Some(color) => out.push(bg_code(color)),
                        None => out.extend(
                            parts[index..(index + width).min(parts.len())]
                                .iter()
                                .map(|part| part.to_string()),
                        ),
                    }
                    index += width;
                    continue;
                }
                Some(n @ 30..=37) => out.push(fg_code(self.ansi[(n - 30) as usize])),
                Some(n @ 90..=97) => out.push(fg_code(self.ansi[(n - 90 + 8) as usize])),
                Some(n @ 40..=47) => out.push(bg_code(self.ansi[(n - 40) as usize])),
                Some(n @ 100..=107) => out.push(bg_code(self.ansi[(n - 100 + 8) as usize])),
                Some(39) => out.push(fg_code(self.fg)),
                Some(49) => out.push(bg_code(self.bg)),
                _ => out.push(parts[index].to_string()),
            }
            index += 1;
        }
        out.join(";")
    }
}

/// Colours for the live-area chrome the renderer draws itself: the prompt
/// and active markers (`accent`), and rules, status text, ghost text and
/// idle markers (`muted`). Each is an opening SGR run; the planner closes
/// with a reset. The `Default` value is the pre-theme fixed pair, kept for
/// planners with no scheme at hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeStyle {
    pub accent: String,
    pub muted: String,
}

impl Default for ChromeStyle {
    fn default() -> Self {
        Self {
            accent: crossterm::style::SetForegroundColor(crossterm::style::Color::Cyan).to_string(),
            muted: crossterm::style::SetForegroundColor(crossterm::style::Color::DarkGrey)
                .to_string(),
        }
    }
}

impl ChromeStyle {
    pub fn from_scheme(colors: &ColorScheme) -> Self {
        let open = |spec: &finch_theme::ColorSpec| {
            format!("\x1b[{}m", fg_code(span_color_from_spec(spec)))
        };
        Self {
            accent: open(&colors.ui.cursor),
            muted: open(&colors.ui.separator),
        }
    }
}

/// The transcript drag-selection highlight (#221): bold `highlight_fg` on
/// `highlight_bg` from the user's scheme. Callers lower it through the same
/// [`lower_span`] every other transcript style uses; nothing here writes raw
/// SGR.
pub fn selection_highlight_style(colors: &ColorScheme) -> SpanStyle {
    SpanStyle::fg(span_color_from_spec(&colors.highlight_fg))
        .with_bg(span_color_from_spec(&colors.highlight_bg))
        .with_bold(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use finch_ui_model::{Span, SpanStyle};

    /// The lowering emits the byte shape the retired `wizard_paint`-era
    /// helpers used — attributes then text then reset — so unchanged styles
    /// render byte-identically.
    #[test]
    fn test_lower_span_emits_attributes_text_and_reset() {
        let span = Span::styled("hi", SpanStyle::fg(SpanColor::DARK_CYAN).with_bold(true));
        assert_eq!(
            lower_span(&span),
            "\x1b[1;36mhi\x1b[0m",
            "bold then colour, closed by a reset; got {:?}",
            lower_span(&span)
        );
        assert_eq!(lower_span(&Span::plain("plain")), "plain");
    }

    /// The xterm bright half lowers to the 90–97/100–107 run and a
    /// background rides the same attribute run as the foreground.
    #[test]
    fn test_lower_span_covers_bright_colours_and_backgrounds() {
        let span = Span::styled(
            "selected",
            SpanStyle::fg(SpanColor::WHITE)
                .with_bg(SpanColor::BLACK)
                .with_bold(true),
        );
        assert_eq!(
            lower_span(&span),
            "\x1b[1;97;40mselected\x1b[0m",
            "bright white on black with bold; got {:?}",
            lower_span(&span)
        );
        let dim = Span::styled("…", SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true));
        assert_eq!(
            lower_span(&dim),
            "\x1b[2;90m…\x1b[0m",
            "dim dark grey (the retired GrayDim); got {:?}",
            lower_span(&dim)
        );
        let rgb = Span::styled("x", SpanStyle::fg(SpanColor::Rgb(1, 2, 3)));
        assert_eq!(lower_span(&rgb), "\x1b[38;2;1;2;3mx\x1b[0m");
    }

    /// A rendered line lowers its spans; a span-free line keeps its legacy
    /// bytes exactly.
    #[test]
    fn test_lower_rendered_line_prefers_spans_and_keeps_plain_bytes() {
        let plain = RenderedTranscriptLine {
            text: "legacy \x1b[2mdim\x1b[0m".to_string(),
            ..RenderedTranscriptLine::default()
        };
        assert_eq!(
            lower_rendered_line(&plain, None, None),
            "legacy \x1b[2mdim\x1b[0m",
            "span-free lines pass their bytes through untouched"
        );
        let styled = RenderedTranscriptLine::from_spans(vec![
            Span::styled("⏺", SpanStyle::fg(SpanColor::CYAN)),
            Span::plain(" Generating"),
        ]);
        assert_eq!(
            lower_rendered_line(&styled, None, None),
            "\x1b[96m⏺\x1b[0m Generating"
        );
    }

    /// When a line carries a background style (e.g. user turns or hovered rows),
    /// the background is extended to the right margin with \x1b[K before resetting.
    #[test]
    fn test_lower_rendered_line_extends_background_across_row() {
        let user_line = RenderedTranscriptLine::from_spans(vec![Span::styled(
            " ❯ hello",
            SpanStyle::fg(SpanColor::CYAN).with_bg(SpanColor::Rgb(38, 38, 42)),
        )]);
        let lowered = lower_rendered_line(&user_line, None, None);
        assert!(
            lowered.ends_with("\x1b[K\x1b[0m"),
            "line with background must extend with \\x1b[K before reset; got {lowered:?}"
        );

        let hover_line = RenderedTranscriptLine::from_spans(vec![Span::plain("item")]);
        let hovered = lower_rendered_line(&hover_line, Some(SpanColor::Rgb(52, 54, 60)), None);
        assert!(
            hovered.ends_with("\x1b[K\x1b[0m"),
            "hovered line must extend background with \\x1b[K before reset; got {hovered:?}"
        );
    }

    /// The selection highlight lowers through the same span path as any
    /// other style — a fixed bold-white-on-blue run, not a hand-written
    /// escape sequence.
    #[test]
    fn test_selection_highlight_style_lowers_through_the_span_path() {
        let span = Span::styled("hi", selection_highlight_style(&ColorScheme::default()));
        assert_eq!(
            lower_span(&span),
            "\x1b[1;97;44mhi\x1b[0m",
            "bold bright white (97) on blue (44); got {:?}",
            lower_span(&span)
        );
    }

    /// The scheme bridge: the Dark default scheme maps the progress and
    /// static roles to the scheme's own colours while the glyph vocabulary
    /// keeps the pre-migration fixed values.
    #[test]
    fn test_component_style_palette_maps_the_scheme_roles() {
        let palette = component_style_palette(&ColorScheme::default());
        assert_eq!(
            palette.progress_running,
            SpanStyle::fg(SpanColor::DARK_YELLOW),
            "status.operation is dark yellow in the default scheme; got {:?}",
            palette.progress_running
        );
        assert_eq!(
            palette.progress_complete,
            SpanStyle::fg(SpanColor::DARK_CYAN),
            "status.download maps the complete bar"
        );
        assert_eq!(
            palette.progress_failed,
            SpanStyle::fg(SpanColor::DARK_RED),
            "messages.error maps the failed bar"
        );
        assert_eq!(
            palette.operation_glyph,
            SpanStyle::fg(SpanColor::DARK_CYAN),
            "the ⏺ glyph takes the scheme accent (ui.cursor)"
        );
        assert_eq!(
            palette.operation_row_glyph,
            SpanStyle::fg(SpanColor::Rgb(180, 180, 180)),
            "the ⎿ glyph takes the scheme's muted messages.system"
        );
        assert_eq!(
            palette.operation_summary,
            SpanStyle::fg(SpanColor::Rgb(180, 180, 180)).with_dim(true),
            "summaries are the dimmed muted colour"
        );
        assert_eq!(
            palette.operation_error,
            SpanStyle::fg(SpanColor::DARK_RED),
            "row errors take messages.error"
        );
        assert_eq!(
            palette.user_foreground,
            SpanColor::DARK_CYAN,
            "messages.user maps to dark cyan in the default scheme"
        );
        assert_eq!(
            palette.user_background,
            SpanColor::Rgb(24, 24, 27),
            "local user band maps to the softened near-black in the dark scheme"
        );
        assert_eq!(
            palette.hover_background,
            SpanColor::Rgb(52, 56, 68),
            "hover background maps to the softened dark-scheme hover color"
        );
    }

    /// The reported light-theme failure: the transcript was filled with the
    /// scheme background while the composer and status rows below it were
    /// not, and the glyph colours ignored the scheme. Every preset must put
    /// its own canvas and its own accent/muted colours on a chrome row.
    #[test]
    fn test_canvas_and_chrome_follow_every_preset_scheme() {
        use finch_theme::ColorTheme;
        for theme in ColorTheme::all() {
            let scheme = theme.to_scheme();
            let canvas = Canvas::from_scheme(&scheme);
            let chrome = ChromeStyle::from_scheme(&scheme);
            let palette = component_style_palette(&scheme);
            let name = theme.name();

            assert_eq!(
                (canvas.bg, canvas.fg),
                (
                    span_color_from_spec(&scheme.background),
                    span_color_from_spec(&scheme.foreground)
                ),
                "{name}: the canvas is the scheme's background and foreground"
            );
            assert_ne!(
                canvas.bg, canvas.fg,
                "{name}: text must not be painted in its own background colour"
            );
            assert_eq!(
                chrome.accent,
                format!("\x1b[{}m", fg_code(span_color_from_spec(&scheme.ui.cursor))),
                "{name}: the prompt accent is ui.cursor"
            );
            assert_eq!(
                chrome.muted,
                format!(
                    "\x1b[{}m",
                    fg_code(span_color_from_spec(&scheme.ui.separator))
                ),
                "{name}: rules and status text are ui.separator"
            );
            assert_eq!(
                palette.operation_glyph,
                SpanStyle::fg(span_color_from_spec(&scheme.ui.cursor)),
                "{name}: the ⏺ glyph is the scheme accent, not a fixed cyan"
            );

            let row = format!("{}❯{SGR_RESET} typed", chrome.accent);
            let painted = canvas.paint_row(&row);
            let open = canvas.open();
            assert!(
                painted.starts_with(&format!("{open}\x1b[K")),
                "{name}: a row opens on the canvas and erases to the margin before its text; painted={painted:?}"
            );
            assert!(
                painted.contains(&format!("{SGR_RESET}{open} typed")),
                "{name}: text after a styled fragment returns to the canvas, not the terminal default; painted={painted:?}"
            );
            assert!(
                !painted.trim_end_matches(SGR_RESET).ends_with("\x1b[K"),
                "{name}: erasing after the text deletes the last column of a full-width row; painted={painted:?}"
            );
        }
    }

    /// Colour that a producer wrote as a bare ANSI code, in any of the forms
    /// the codebase emits (crossterm's `38;5;n`, the short `3x`/`9x` codes,
    /// and `39` "default foreground"), must reach the terminal as the
    /// scheme's colour. This is what keeps tool labels, dialog rows, diff
    /// messages and markdown code readable on the light theme without each
    /// producer knowing the scheme.
    #[test]
    fn test_paint_row_maps_bare_ansi_colours_onto_the_scheme() {
        let light = finch_theme::ColorTheme::Light.to_scheme();
        let canvas = Canvas::from_scheme(&light);
        let accent = fg_code(span_color_from_spec(&light.ui.cursor));
        let muted = fg_code(span_color_from_spec(&light.messages.system));
        let error = fg_code(span_color_from_spec(&light.messages.error));
        let ink = fg_code(canvas.fg);

        for (legacy, expected, what) in [
            (
                "\x1b[38;5;14m",
                accent.as_str(),
                "crossterm bright cyan (the old fixed accent)",
            ),
            (
                "\x1b[36m",
                accent.as_str(),
                "short-form cyan (markdown inline code)",
            ),
            (
                "\x1b[38;5;8m",
                muted.as_str(),
                "crossterm dark grey (the old fixed muted)",
            ),
            ("\x1b[38;5;9m", error.as_str(), "crossterm red"),
            (
                "\x1b[38;5;15m",
                ink.as_str(),
                "white text, which would vanish on a white canvas",
            ),
            ("\x1b[39m", ink.as_str(), "default-foreground reset"),
        ] {
            let painted = canvas.paint_row(&format!("{legacy}x"));
            assert!(
                painted.contains(&format!("\x1b[{expected}mx")),
                "Light: {what} must paint as the scheme colour {expected:?}; painted={painted:?}"
            );
        }

        let bold = canvas.paint_row("\x1b[1m\x1b[38;5;14mGrep\x1b[0m(args)");
        assert!(
            bold.contains("\x1b[1m") && bold.contains(&format!("\x1b[{accent}mGrep")),
            "modifiers survive and the colour follows the scheme; painted={bold:?}"
        );
        let truecolor = canvas.paint_row("\x1b[38;2;1;2;3;48;2;4;5;6mx");
        assert!(
            truecolor.contains("\x1b[38;2;1;2;3;48;2;4;5;6mx"),
            "a colour given by value is already a scheme colour and passes through; painted={truecolor:?}"
        );
        let extended = canvas.paint_row("\x1b[38;5;200mx");
        assert!(
            extended.contains("\x1b[38;5;200mx"),
            "a 256-colour index past the ANSI 16 is not a named colour; painted={extended:?}"
        );

        // Dark uses cyan by name, so its own cyan is unchanged.
        let dark = Canvas::from_scheme(&finch_theme::ColorTheme::Dark.to_scheme());
        assert!(
            dark.paint_row("\x1b[36mx").contains("\x1b[36mx"),
            "a named colour the scheme itself uses keeps meaning itself"
        );
    }

    /// The light preset on a light scheme must not resolve to the dark
    /// defaults: its canvas is white with dark ink.
    #[test]
    fn test_light_scheme_canvas_is_dark_ink_on_white() {
        let canvas = Canvas::from_scheme(&finch_theme::ColorTheme::Light.to_scheme());
        assert_eq!(canvas.bg, SpanColor::Rgb(255, 255, 255));
        assert_eq!(canvas.fg, SpanColor::Rgb(31, 35, 40));
    }

    /// User turn foreground and background bridge correctly across all themes.
    #[test]
    fn test_component_style_palette_maps_user_roles_across_themes() {
        use finch_theme::ColorTheme;
        for theme in ColorTheme::all() {
            let scheme = theme.to_scheme();
            let palette = component_style_palette(&scheme);

            let expected_band = scheme.message_band_style(MessageBand::LocalUser);
            let expected_bg = match expected_band.bg.unwrap() {
                ratatui::style::Color::Rgb(r, g, b) => SpanColor::Rgb(r, g, b),
                _ => panic!("expected RGB background"),
            };
            assert_eq!(palette.user_background, expected_bg);

            for i in 0..8 {
                let expected_part_band = scheme.message_band_style(MessageBand::Participant(i));
                let expected_part_bg = match expected_part_band.bg.unwrap() {
                    ratatui::style::Color::Rgb(r, g, b) => SpanColor::Rgb(r, g, b),
                    _ => panic!("expected RGB background"),
                };
                assert_eq!(palette.participant_backgrounds[i], expected_part_bg);
            }

            let expected_hover = match scheme.hover_background() {
                ratatui::style::Color::Rgb(r, g, b) => SpanColor::Rgb(r, g, b),
                _ => panic!("expected RGB hover background"),
            };
            assert_eq!(palette.hover_background, expected_hover);
        }
    }
}
