//! The style-span vocabulary: semantic (text, style) segments without any
//! terminal bytes (stage 4 of docs/TUI_DESIGN.md).
//!
//! A rendered line is a sequence of [`Span`]s — each a text fragment plus an
//! optional foreground/background colour and modifiers — instead of a
//! pre-styled SGR string. Components build spans from a plain style palette;
//! each render mode lowers them itself (terminal → SGR at paint, DOM → styled
//! elements). Nothing in this module emits escape sequences: the lowering
//! belongs to the render engines, so "one tree, two lowerings" stays true.

/// A terminal colour, independent of any renderer's palette.
///
/// `Indexed` is the xterm 16-colour set (0–15, the crossterm table: 0–7 the
/// normal set, 8–15 the bright set). Render engines lower it to their own
/// codes; the DOM maps it to CSS colours. `Rgb` carries a truecolour triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanColor {
    /// xterm palette index 0–15.
    Indexed(u8),
    /// 24-bit colour.
    Rgb(u8, u8, u8),
}

impl SpanColor {
    pub const BLACK: SpanColor = SpanColor::Indexed(0);
    pub const DARK_RED: SpanColor = SpanColor::Indexed(1);
    pub const DARK_GREEN: SpanColor = SpanColor::Indexed(2);
    pub const DARK_YELLOW: SpanColor = SpanColor::Indexed(3);
    pub const DARK_BLUE: SpanColor = SpanColor::Indexed(4);
    pub const DARK_MAGENTA: SpanColor = SpanColor::Indexed(5);
    pub const DARK_CYAN: SpanColor = SpanColor::Indexed(6);
    pub const GREY: SpanColor = SpanColor::Indexed(7);
    pub const DARK_GREY: SpanColor = SpanColor::Indexed(8);
    pub const RED: SpanColor = SpanColor::Indexed(9);
    pub const GREEN: SpanColor = SpanColor::Indexed(10);
    pub const YELLOW: SpanColor = SpanColor::Indexed(11);
    pub const BLUE: SpanColor = SpanColor::Indexed(12);
    pub const MAGENTA: SpanColor = SpanColor::Indexed(13);
    pub const CYAN: SpanColor = SpanColor::Indexed(14);
    pub const WHITE: SpanColor = SpanColor::Indexed(15);
}

/// The style of one [`Span`]: optional colours plus modifiers. A plain
/// (default) style carries no colour and no modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    pub fg: Option<SpanColor>,
    pub bg: Option<SpanColor>,
    pub bold: bool,
    /// SGR 2 dim — the faded modifier the retired `format()` paths used for
    /// running ellipses and summaries.
    pub dim: bool,
}

impl SpanStyle {
    /// No colour, no modifier.
    pub const PLAIN: SpanStyle = SpanStyle {
        fg: None,
        bg: None,
        bold: false,
        dim: false,
    };

    /// Foreground colour only.
    pub const fn fg(color: SpanColor) -> SpanStyle {
        SpanStyle {
            fg: Some(color),
            bg: None,
            bold: false,
            dim: false,
        }
    }

    /// Set the foreground colour, keeping the other attributes.
    pub const fn with_fg(mut self, color: SpanColor) -> SpanStyle {
        self.fg = Some(color);
        self
    }

    /// Set the background colour, keeping the other attributes.
    pub const fn with_bg(mut self, color: SpanColor) -> SpanStyle {
        self.bg = Some(color);
        self
    }

    /// Set or clear bold, keeping the other attributes.
    pub const fn with_bold(mut self, bold: bool) -> SpanStyle {
        self.bold = bold;
        self
    }

    /// Set or clear dim, keeping the other attributes.
    pub const fn with_dim(mut self, dim: bool) -> SpanStyle {
        self.dim = dim;
        self
    }

    /// True when the style changes nothing about how text renders.
    pub const fn is_plain(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && !self.bold && !self.dim
    }
}

/// One (text, style) segment of a rendered line. Adjacent plain-text
/// neighbours stay separate spans; render engines may merge for paint
/// efficiency but must not change the visible text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
}

impl Span {
    /// A span with no styling.
    pub fn plain(text: impl Into<String>) -> Span {
        Span {
            text: text.into(),
            style: SpanStyle::PLAIN,
        }
    }

    /// A styled span.
    pub fn styled(text: impl Into<String>, style: SpanStyle) -> Span {
        Span {
            text: text.into(),
            style,
        }
    }

    /// The visible text, which is what measurement and the canonical record
    /// read — never any terminal bytes.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Concatenate span texts to the line's plain text. Component renderers and
/// the engines that lower spans use this, so the plain projection and the
/// styled one can never disagree about content.
pub fn spans_text(spans: &[Span]) -> String {
    let mut text = String::new();
    for span in spans {
        text.push_str(&span.text);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spans_text_concatenates_segments_in_order() {
        // The plain projection of a span line is exactly the concatenation of
        // its segment texts — no separator, no styling artefacts — so
        // measurement and the canonical record stay byte-identical to the
        // pre-span rendering.
        let spans = vec![
            Span::styled("⏺ ", SpanStyle::fg(SpanColor::CYAN)),
            Span::plain("Generating"),
            Span::styled("…", SpanStyle::PLAIN.with_dim(true)),
        ];
        assert_eq!(
            spans_text(&spans),
            "⏺ Generating…",
            "spans_text must concatenate segment texts in order; got spans {spans:?}"
        );
    }

    #[test]
    fn test_span_style_builders_keep_other_attributes() {
        // INVARIANT (#1141): style builders are additive, so a role style can
        // gain a background (the #1140 selection contrast) without losing the
        // foreground or modifiers the base style already carried.
        let style = SpanStyle::fg(SpanColor::WHITE)
            .with_bold(true)
            .with_bg(SpanColor::BLACK);
        assert_eq!(
            style,
            SpanStyle {
                fg: Some(SpanColor::WHITE),
                bg: Some(SpanColor::BLACK),
                bold: true,
                dim: false,
            },
            "fg + bold + bg must compose; got {style:?}"
        );
        assert!(!style.is_plain(), "a coloured style is not plain");
        assert!(SpanStyle::PLAIN.is_plain(), "the default style is plain");
    }
}
