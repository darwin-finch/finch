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
fn fg_code(color: SpanColor) -> String {
    match color {
        SpanColor::Indexed(index) if index < 8 => format!("{}", 30 + index),
        SpanColor::Indexed(index) => format!("{}", 90 + (index - 8)),
        SpanColor::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
    }
}

/// SGR parameter for one colour: background position.
fn bg_code(color: SpanColor) -> String {
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
    canvas_fg: Option<SpanColor>,
) -> String {
    if let Some(b64) = &line.image_attachment {
        if std::env::var("TERM_PROGRAM").as_deref() == Ok("iTerm.app") {
            return format!("\x1b]1337;File=inline=1;width=auto;height=auto:{}\x07", b64);
        }
    }

    let mut rendered = if line.spans.is_empty() {
        let bg = force_bg.clone().or_else(|| canvas_bg.clone());
        if bg.is_some() || canvas_fg.is_some() {
            let mut style = SpanStyle::default();
            if let Some(bg) = bg { style.bg = Some(bg); }
            if let Some(fg) = canvas_fg.clone() { style.fg = Some(fg); }
            let span = Span::styled(&line.text, style);
            lower_span(&span)
        } else {
            line.text.clone()
        }
    } else {
        if force_bg.is_some() || canvas_bg.is_some() || canvas_fg.is_some() {
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
                    if style.fg.is_none() {
                        if let Some(cfg) = &canvas_fg {
                            style.fg = Some(cfg.clone());
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

    let extends_bg = canvas_bg.is_some() || line.spans.iter().any(|s| s.style.bg.is_some());
    if extends_bg {
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
/// vocabulary's colour, with the same named-colour table the retired
/// `format()` paths used.
pub(crate) fn span_color_from_spec(spec: &finch_theme::ColorSpec) -> SpanColor {
    match spec {
        finch_theme::ColorSpec::Rgb(r, g, b) => SpanColor::Rgb(*r, *g, *b),
        finch_theme::ColorSpec::Named(name) => match name.to_lowercase().as_str() {
            "black" => SpanColor::BLACK,
            "red" => SpanColor::DARK_RED,
            "green" => SpanColor::DARK_GREEN,
            "yellow" => SpanColor::DARK_YELLOW,
            "blue" => SpanColor::DARK_BLUE,
            "magenta" => SpanColor::DARK_MAGENTA,
            "cyan" => SpanColor::DARK_CYAN,
            "white" => SpanColor::WHITE,
            "gray" | "grey" | "darkgray" | "darkgrey" => SpanColor::DARK_GREY,
            "lightred" => SpanColor::RED,
            "lightgreen" => SpanColor::GREEN,
            "lightyellow" => SpanColor::YELLOW,
            "lightblue" => SpanColor::BLUE,
            "lightmagenta" => SpanColor::MAGENTA,
            "lightcyan" => SpanColor::CYAN,
            _ => SpanColor::GREY,
        },
    }
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

/// Build the component palette from the user's scheme: the roles the scheme
/// owns (progress, static-row, and user-turn colours) come from it; the glyph vocabulary
/// (⏺ ⎿ dim summaries) keeps the pre-migration fixed colours. This is the one
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

/// The transcript drag-selection highlight (#221): bold bright white on
/// blue, the common terminal/editor selection convention, mirroring the
/// wizard's own selection-contrast precedent (#1140, `wizard_selected` in
/// `wizard_host.rs`) rather than inventing a new look. A fixed colour pair,
/// not derived from the user's `ColorScheme` — selection has no natural
/// semantic role in that scheme, so this stays a named constant instead of
/// a fabricated mapping. Callers lower it through the same [`lower_span`]
/// every other transcript style uses; nothing here writes raw SGR.
pub fn selection_highlight_style() -> SpanStyle {
    SpanStyle::fg(SpanColor::WHITE)
        .with_bg(SpanColor::DARK_BLUE)
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
            lower_rendered_line(&plain, None, None, None),
            "legacy \x1b[2mdim\x1b[0m",
            "span-free lines pass their bytes through untouched"
        );
        let styled = RenderedTranscriptLine::from_spans(vec![
            Span::styled("⏺", SpanStyle::fg(SpanColor::CYAN)),
            Span::plain(" Generating"),
        ]);
        assert_eq!(
            lower_rendered_line(&styled, None, None, None),
            "\x1b[96m⏺\x1b[0m Generating"
        );
    }

    /// When a line carries a background style (e.g. user turns or canvas background),
    /// the background is extended to the right margin with \x1b[K before resetting.
    /// However, forced backgrounds (like hover) do NOT extend.
    #[test]
    fn test_lower_rendered_line_extends_background_across_row() {
        let user_line = RenderedTranscriptLine::from_spans(vec![Span::styled(
            " ❯ hello",
            SpanStyle::fg(SpanColor::CYAN).with_bg(SpanColor::Rgb(38, 38, 42)),
        )]);
        let lowered = lower_rendered_line(&user_line, None, None, None);
        assert!(
            lowered.ends_with("\x1b[K\x1b[0m"),
            "line with background must extend with \\x1b[K before reset; got {lowered:?}"
        );

        let hover_line = RenderedTranscriptLine::from_spans(vec![Span::plain("item")]);
        let hovered = lower_rendered_line(&hover_line, Some(SpanColor::Rgb(52, 54, 60)), None, None);
        assert!(
            !hovered.ends_with("\x1b[K\x1b[0m"),
            "hovered line must NOT extend background with \\x1b[K before reset; got {hovered:?}"
        );

        let canvas_line = RenderedTranscriptLine::from_spans(vec![Span::plain("item")]);
        let canvas_lowered =
            lower_rendered_line(&canvas_line, None, Some(SpanColor::Rgb(52, 54, 60)), None);
        assert!(
            canvas_lowered.ends_with("\x1b[K\x1b[0m"),
            "canvas background must extend with \\x1b[K before reset; got {canvas_lowered:?}"
        );
    }

    /// The selection highlight lowers through the same span path as any
    /// other style — a fixed bold-white-on-blue run, not a hand-written
    /// escape sequence.
    #[test]
    fn test_selection_highlight_style_lowers_through_the_span_path() {
        let span = Span::styled("hi", selection_highlight_style());
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
            SpanStyle::fg(SpanColor::CYAN),
            "the ⏺ glyph keeps the retained cyan"
        );
        assert_eq!(
            palette.operation_row_glyph,
            SpanStyle::fg(SpanColor::DARK_GREY),
            "the ⎿ glyph keeps the retained dark grey"
        );
        assert_eq!(
            palette.operation_summary,
            SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
            "summaries keep the dimmed dark grey"
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
            SpanColor::Rgb(36, 38, 42),
            "hover background maps to the softened dark-scheme hover color"
        );
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
