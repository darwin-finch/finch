//! Lexical facts of the production CoLisp reader.
//!
//! The published compact-wire grammar is generated from these constants so a
//! comment marker, string delimiter, or quote character cannot change in the
//! tokenizer without changing the machine-readable grammar.

/// Surface-syntax facts consumed by the CoLisp tokenizer and the wire GBNF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LispLexicon {
    /// `;` starts a line comment that runs through the following newline.
    pub line_comment: char,
    /// `#|` opens a non-nesting block comment.
    pub block_comment_open: &'static str,
    /// `|#` closes a block comment.
    pub block_comment_close: &'static str,
    /// `"` opens and closes an escaped string.
    pub string_delimiter: char,
    /// `$` wraps a math expression that the reader expands.
    pub math_delimiter: char,
    /// `'` is quote sugar.
    pub quote: char,
    /// `` ` `` is quasiquote sugar.
    pub quasiquote: char,
    /// `,` is unquote sugar; `,@` is unquote-splicing.
    pub unquote: char,
    /// `,@` is unquote-splicing sugar.
    pub unquote_splice: &'static str,
    /// `(` opens a list.
    pub list_open: char,
    /// `)` closes a list.
    pub list_close: char,
    /// `{` opens a typed record or a JSON object.
    pub record_open: char,
    /// `}` closes a typed record or a JSON object.
    pub record_close: char,
    /// `[` opens a JSON array literal.
    pub json_array_open: char,
    /// `]` closes a JSON array literal.
    pub json_array_close: char,
    /// Compact structural types (`record{...}`, `variant{...}`) are atoms.
    pub compact_type_prefixes: &'static [&'static str],
    /// Recognized string escape letters; unknown escapes keep the backslash.
    pub string_escape_letters: &'static str,
}

/// Production CoLisp lexical facts. The tokenizer must use this value.
pub fn lisp_lexicon() -> LispLexicon {
    LispLexicon {
        line_comment: ';',
        block_comment_open: "#|",
        block_comment_close: "|#",
        string_delimiter: '"',
        math_delimiter: '$',
        quote: '\'',
        quasiquote: '`',
        unquote: ',',
        unquote_splice: ",@",
        list_open: '(',
        list_close: ')',
        record_open: '{',
        record_close: '}',
        json_array_open: '[',
        json_array_close: ']',
        compact_type_prefixes: &["record{", "variant{"],
        string_escape_letters: "ntr\"\\0",
    }
}

/// True when `ch` ends a CoLisp atom outside a `<...>` type argument list.
pub(crate) fn lisp_atom_delimiter(ch: char, angle_depth: usize) -> bool {
    let lex = lisp_lexicon();
    ch.is_whitespace()
        || ch == lex.list_open
        || ch == lex.list_close
        || ch == lex.record_open
        || ch == lex.record_close
        || ch == lex.string_delimiter
        || ch == lex.line_comment
        || ch == lex.quote
        || ch == lex.quasiquote
        || (ch == lex.unquote && angle_depth == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexicon_comment_and_string_markers_are_the_published_surface() {
        let lex = lisp_lexicon();
        assert_eq!(lex.line_comment, ';');
        assert_eq!(lex.block_comment_open, "#|");
        assert_eq!(lex.block_comment_close, "|#");
        assert_eq!(lex.string_delimiter, '"');
        assert_eq!(lex.list_open, '(');
        assert!(
            !lex.string_escape_letters.is_empty(),
            "the published grammar needs the escape letters the tokenizer consumes"
        );
    }
}
