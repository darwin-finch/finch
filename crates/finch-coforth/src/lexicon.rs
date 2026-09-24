//! Lexical facts of the production Co-Forth reader.
//!
//! The published compact-wire grammar is generated from these constants so a
//! string opener, comment marker, or delimiter cannot change in the tokenizer
//! without changing the machine-readable grammar.

/// One Co-Forth string/raw-string opener as recognized by the tokenizer.
///
/// Re-exported from `finch-vm-core`, the single canonical copy: any lighter-
/// weight surface check elsewhere (a syntax-vs-prose classifier, a CLI
/// heuristic) reads the same spellings from there too, instead of hand-
/// maintaining an independent copy that can silently drift out of sync with
/// what this tokenizer actually accepts.
pub use finch_vm_core::ForthStringOpener;

/// Surface-syntax facts consumed by the Co-Forth tokenizer and the wire GBNF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForthLexicon {
    /// `\` starts a line comment that runs through the following newline.
    pub line_comment: char,
    /// Commas are optional collection separators and are otherwise ignored.
    pub comma_separator: char,
    /// Raw-string closer, matching `"""` / `s"""` bodies.
    pub raw_string_close: &'static str,
    /// String openers in tokenizer precedence (longest match first).
    pub string_openers: &'static [ForthStringOpener],
    /// `[']` is the quotation-target token, recognized before `[`.
    pub quote_word: &'static str,
    /// Compact structural types are one token: `record{...}`, `variant{...}`.
    pub compact_type_prefixes: &'static [&'static str],
    /// Parameterized type applications are one token even with commas.
    pub parameterized_type_prefixes: &'static [&'static str],
    /// Collection literal openers that are not compact types.
    pub collection_openers: &'static [&'static str],
    /// Matching collection literal closers.
    pub collection_closers: &'static [&'static str],
    /// `[` opens a list or a quotation.
    pub list_open: char,
    /// `]` closes a list or a quotation.
    pub list_close: char,
    /// `{` opens a typed record (when it is not pasted JSON).
    pub record_open: char,
    /// `}` closes a typed record.
    pub record_close: char,
    /// `:` starts a typed word definition.
    pub definition_start: &'static str,
    /// `;` ends a typed word definition.
    pub definition_end: &'static str,
    /// `(` opens a typed definition signature.
    pub signature_open: &'static str,
    /// `)` closes a typed definition signature.
    pub signature_close: &'static str,
    /// `--` marks a quotation output row.
    pub quotation_arrow: &'static str,
    /// `|` separates a quotation signature from its body.
    pub quotation_pipe: &'static str,
}

/// Production Co-Forth lexical facts. The tokenizer must use this value.
pub fn forth_lexicon() -> ForthLexicon {
    ForthLexicon {
        line_comment: '\\',
        comma_separator: ',',
        raw_string_close: "\"\"\"",
        string_openers: finch_vm_core::FORTH_STRING_OPENERS,
        quote_word: "[']",
        compact_type_prefixes: &["record{", "variant{"],
        parameterized_type_prefixes: &[
            "empty-list<",
            "empty-map<",
            "list<",
            "map<",
            "option<",
            "result<",
            "fn<",
            "task<",
            "fiber<",
            "stream<",
            "resource<",
            "capability<",
            "variant<",
            "variant-get<",
        ],
        collection_openers: &["map{", "list{", "record{"],
        collection_closers: &["}map", "}list", "}record"],
        list_open: '[',
        list_close: ']',
        record_open: '{',
        record_close: '}',
        definition_start: ":",
        definition_end: ";",
        signature_open: "(",
        signature_close: ")",
        quotation_arrow: "--",
        quotation_pipe: "|",
    }
}

/// Bytes that end a Co-Forth word in the generic (non-type) token loop.
pub(crate) fn forth_word_terminator(byte: u8) -> bool {
    let lex = forth_lexicon();
    byte.is_ascii_whitespace()
        || byte == lex.comma_separator as u8
        || byte == lex.list_open as u8
        || byte == lex.list_close as u8
        || byte == lex.record_close as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_openers_are_longest_first_and_include_raw_and_escaped_forms() {
        let lex = forth_lexicon();
        let spellings: Vec<_> = lex
            .string_openers
            .iter()
            .map(|opener| opener.spelling)
            .collect();
        assert_eq!(
            spellings,
            ["s\"\"\"", "\"\"\"", ".\"", "s\"", "\""],
            "tokenizer precedence is the published opener list, longest first: {spellings:?}"
        );
        assert!(
            lex.string_openers.iter().any(|opener| opener.raw),
            "raw triple-quote literals must remain in the lexicon so the wire grammar can emit them"
        );
        assert_eq!(lex.line_comment, '\\');
        assert_eq!(lex.raw_string_close, "\"\"\"");
    }
}
