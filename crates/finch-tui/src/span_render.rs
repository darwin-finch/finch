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

use finch_theme::ColorScheme;
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
    if let Some(fg) = style.fg {
        codes.push(fg_code(fg));
    }
    if let Some(bg) = style.bg {
        codes.push(bg_code(bg));
    }
    codes.join(";")
}

const SGR_RESET: &str = "\x1b[0m";

/// Lower one span to the bytes a terminal paints: the style run, the span's
/// text, and a reset. A plain span is the text alone, so unstyled content
/// reaches the terminal exactly as the legacy paths wrote it.
pub fn lower_span(span: &Span) -> String {
    let codes = style_codes(&span.style);
    if codes.is_empty() {
        span.text.clone()
    } else {
        format!("\x1b[{codes}m{}{SGR_RESET}", span.text)
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
pub fn lower_rendered_line(line: &RenderedTranscriptLine) -> String {
    if line.spans.is_empty() {
        line.text.clone()
    } else {
        lower_spans(&line.spans)
    }
}

/// Map a `ColorSpec` (the ColorScheme's serializable colour) to the span
/// vocabulary's colour, with the same named-colour table the retired
/// `format()` paths used.
fn span_color_from_spec(spec: &finch_theme::ColorSpec) -> SpanColor {
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
            "white" => SpanColor::GREY,
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

/// Build the component palette from the user's scheme: the roles the scheme
/// owns (progress and static-row colours) come from it; the glyph vocabulary
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
    palette.static_success = palette.static_info;
    palette.static_warning = SpanStyle::fg(span_color_from_spec(&colors.status.operation));
    palette
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
            lower_rendered_line(&plain),
            "legacy \x1b[2mdim\x1b[0m",
            "span-free lines pass their bytes through untouched"
        );
        let styled = RenderedTranscriptLine::from_spans(vec![
            Span::styled("⏺", SpanStyle::fg(SpanColor::CYAN)),
            Span::plain(" Generating"),
        ]);
        assert_eq!(lower_rendered_line(&styled), "\x1b[96m⏺\x1b[0m Generating");
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
    }
}
