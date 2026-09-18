//! Bounded assistant-prose markdown rendering for the transcript viewport.
//!
//! One parse inside the one domain→widget projection (#756): assistant prose
//! is parsed once into a small block model — fenced code blocks, emphasis,
//! inline code, and lists — and rendered to styled body lines for the live
//! viewport. The canonical commit renders the RAW source lines instead (the
//! copyable record per `docs/TUI_ARCHITECTURE.md`), so this renderer never
//! feeds native scrollback.
//!
//! The construct set is deliberately bounded (contract decision on #756: no
//! markdown crate): fenced code blocks, bold/italic emphasis, inline code, and
//! ordered/unordered list items. Anything else — headings, tables, links,
//! blockquotes — and any malformed input passes through as literal paragraph
//! text without panicking: every delimiter that does not close cleanly is
//! emitted as ordinary characters, and an unterminated fence renders as an
//! (open) code block so streaming output stays readable mid-block.
//!
//! Text semantics are preserved for assistive reading: the renderer changes
//! markers only where the construct is carried elsewhere (bold/italic become
//! SGR attributes; the raw body the canonical commit writes keeps the source
//! markers), and code-block bodies stay whitespace-exact byte-for-byte.

/// Inline spans of one source line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Span {
    /// Literal text.
    Text(String),
    /// Bold emphasis (`**…**` / `__…__`); the payload is recursively parsed.
    Bold(Vec<Span>),
    /// Italic emphasis (`*…*` / `_…_`); the payload is recursively parsed.
    Italic(Vec<Span>),
    /// Inline code (`` `…` ``) — literal payload.
    Code(String),
}

/// One block-level construct of the bounded subset.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    /// One source line's inline content (no paragraph joining, no re-wrapping:
    /// the row model's line economics and wrapping stay unchanged).
    Paragraph(Vec<Span>),
    /// A fenced code block. `fence_line` and `closing` are the fence lines
    /// verbatim (`closing` absent while the fence never closed); `lines` are
    /// the interior lines, whitespace-exact.
    FencedCode {
        fence_line: String,
        closing: Option<String>,
        lines: Vec<String>,
    },
    /// One list item. `indent` is the item's original leading whitespace;
    /// `marker` is the rendered marker (`•` for unordered items, the source
    /// numbering for ordered ones).
    ListItem {
        indent: String,
        marker: String,
        spans: Vec<Span>,
    },
}

/// SGR attributes for rendered spans. Bold/dim toggle via their own attribute
/// codes (22/23/39) so nested spans compose instead of resetting each other.
mod sgr {
    pub(crate) const BOLD: &str = "\x1b[1m";
    pub(crate) const BOLD_OFF: &str = "\x1b[22m";
    pub(crate) const ITALIC: &str = "\x1b[3m";
    pub(crate) const ITALIC_OFF: &str = "\x1b[23m";
    pub(crate) const DIM: &str = "\x1b[2m";
    pub(crate) const DIM_OFF: &str = "\x1b[22m";
    pub(crate) const CODE_FG: &str = "\x1b[36m";
    pub(crate) const FG_DEFAULT: &str = "\x1b[39m";
}

/// Recursion budget for nested emphasis. Pathological input (`***…***` nests
/// deeply) is cut off and rendered literally rather than recursing unbounded.
const MAX_INLINE_DEPTH: usize = 8;

/// Render assistant prose into styled viewport body lines.
///
/// This is the one markdown parse in the transcript pipeline (#756). The raw
/// source lines the canonical commit writes are the same text without this
/// rendering — `crate::cli::tui::view_model` carries both on the node.
pub(crate) fn render_viewport_body(source: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for block in parse_blocks(source) {
        match block {
            Block::Paragraph(spans) => lines.push(render_inline(&spans)),
            Block::FencedCode {
                fence_line,
                closing,
                lines: body,
            } => {
                lines.push(format!("{}{}{}", sgr::DIM, fence_line, sgr::DIM_OFF));
                // Body lines stay byte-exact: the code is the copyable content.
                lines.extend(body);
                if let Some(closing) = closing {
                    lines.push(format!("{}{}{}", sgr::DIM, closing, sgr::DIM_OFF));
                }
            }
            Block::ListItem {
                indent,
                marker,
                spans,
            } => lines.push(format!("{indent}{marker} {}", render_inline(&spans))),
        }
    }
    lines
}

/// Parse source text into bounded blocks. `split('\n')` preserves the source's
/// own line structure exactly, including a trailing empty segment.
fn parse_blocks(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open_code: Option<OpenCode> = None;
    for line in source.split('\n') {
        if let Some(code) = open_code.as_mut() {
            if let Some(closing) = closing_fence(line, code.fence_char, code.fence_len) {
                let code = open_code.take().expect("code block is open");
                blocks.push(Block::FencedCode {
                    fence_line: code.fence_line,
                    closing: Some(closing),
                    lines: code.lines,
                });
            } else {
                code.lines.push(line.to_owned());
            }
            continue;
        }
        if let Some((fence_char, fence_len, fence_line)) = opening_fence(line) {
            open_code = Some(OpenCode {
                fence_char,
                fence_len,
                fence_line,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some((indent, marker, rest)) = list_item(line) {
            blocks.push(Block::ListItem {
                indent,
                marker,
                spans: parse_inline(&rest.chars().collect::<Vec<_>>(), MAX_INLINE_DEPTH),
            });
            continue;
        }
        blocks.push(Block::Paragraph(parse_inline(
            &line.chars().collect::<Vec<_>>(),
            MAX_INLINE_DEPTH,
        )));
    }
    // An unterminated fence at end of input (normal while streaming) renders
    // as an open code block: the lines so far are already code-shaped.
    if let Some(code) = open_code {
        blocks.push(Block::FencedCode {
            fence_line: code.fence_line,
            closing: None,
            lines: code.lines,
        });
    }
    blocks
}

/// State of a fence that has opened but not closed yet.
struct OpenCode {
    fence_char: char,
    fence_len: usize,
    fence_line: String,
    lines: Vec<String>,
}

/// The opening fence of a line, if any: three or more backticks or tildes
/// (leading whitespace allowed). Returns the fence character, its run length,
/// and the whole line verbatim (the rendered fence keeps the source text).
fn opening_fence(line: &str) -> Option<(char, usize, String)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|&c| c == first).count();
    if run < 3 {
        return None;
    }
    Some((first, run, line.to_owned()))
}

/// The closing fence of a line: a run of the same fence character at least as
/// long as the opening run, followed by nothing but whitespace. Fence
/// characters are ASCII, so the byte offset of the run equals its char count.
fn closing_fence(line: &str, fence_char: char, fence_len: usize) -> Option<String> {
    let trimmed = line.trim_start();
    let run = trimmed.chars().take_while(|&c| c == fence_char).count();
    if run < fence_len || !trimmed[run..].trim().is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// One list item line: `indent marker rest`, where `marker` is `-`, `*`, `+`
/// or 1–9 digits plus `.`/`)`, always followed by whitespace. The rendered
/// marker is `•` for unordered items and the source numbering (with `)`
/// normalized to `.`) for ordered ones; the original leading whitespace is
/// preserved for nesting.
fn list_item(line: &str) -> Option<(String, String, String)> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = line[..indent_len].to_owned();
    let rest = &line[indent_len..];
    let (marker, marker_len) = match rest.chars().next()? {
        '-' | '*' | '+' => ("•".to_owned(), 1),
        '1'..='9' => {
            let digits = rest
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .count()
                .max(1);
            match rest[digits..].chars().next() {
                Some('.') | Some(')') => (format!("{}.", &rest[..digits]), digits + 1),
                _ => return None,
            }
        }
        _ => return None,
    };
    let after = &rest[marker_len..];
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    Some((indent, marker, after.trim_start().to_owned()))
}

/// Parse one line's inline content into spans. Unmatched delimiters and
/// out-of-budget recursion degrade to literal text.
fn parse_inline(chars: &[char], depth: usize) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '`' => {
                let run = delimiter_run(chars, index, '`');
                match closing_code_span(chars, index, run) {
                    Some((content, after)) if depth > 0 => {
                        push_text(&mut spans, &mut text);
                        spans.push(Span::Code(content.iter().collect()));
                        index = after;
                    }
                    _ => {
                        text.extend(chars[index..index + run].iter());
                        index += run;
                    }
                }
            }
            '*' | '_' => {
                let delimiter = chars[index];
                let run = delimiter_run(chars, index, delimiter);
                let can_open = opens_emphasis(chars, index, run, delimiter);
                let (matched, after) = if can_open && depth > 0 && run >= 2 {
                    match closing_emphasis(chars, index + run, delimiter, 2) {
                        Some((content_end, past_close)) => {
                            let inner = &chars[index + run..content_end];
                            (Some(parse_inline(inner, depth - 1)), past_close)
                        }
                        None => (None, index + run),
                    }
                } else if can_open && depth > 0 {
                    match closing_emphasis(chars, index + run, delimiter, 1) {
                        Some((content_end, past_close)) => {
                            let inner = &chars[index + 1..content_end];
                            (Some(parse_inline(inner, depth - 1)), past_close)
                        }
                        None => (None, index + run),
                    }
                } else {
                    (None, index + run)
                };
                match matched {
                    Some(inner) => {
                        push_text(&mut spans, &mut text);
                        if run >= 2 {
                            spans.push(Span::Bold(inner));
                        } else {
                            spans.push(Span::Italic(inner));
                        }
                        index = after;
                    }
                    None => {
                        text.extend(chars[index..after].iter());
                        index = after;
                    }
                }
            }
            '\\' if index + 1 < chars.len() && chars[index + 1].is_ascii_punctuation() => {
                text.push(chars[index + 1]);
                index += 2;
            }
            other => {
                text.push(other);
                index += 1;
            }
        }
    }
    push_text(&mut spans, &mut text);
    spans
}

fn push_text(spans: &mut Vec<Span>, text: &mut String) {
    if !text.is_empty() {
        spans.push(Span::Text(std::mem::take(text)));
    }
}

/// Length of a run of one delimiter character starting at `index`.
fn delimiter_run(chars: &[char], index: usize, delimiter: char) -> usize {
    chars[index..]
        .iter()
        .take_while(|&&c| c == delimiter)
        .count()
}

/// Matched content and the index just past the closing run of an inline code
/// span: exactly `run_len` backticks. `None` when no matching run exists.
fn closing_code_span(chars: &[char], index: usize, run_len: usize) -> Option<(Vec<char>, usize)> {
    let mut cursor = index + run_len;
    while cursor < chars.len() {
        if chars[cursor] == '`' {
            let run = delimiter_run(chars, cursor, '`');
            if run == run_len {
                return Some((chars[index + run_len..cursor].to_vec(), cursor + run));
            }
            cursor += run;
        } else {
            cursor += 1;
        }
    }
    None
}

/// Whether a delimiter run can open emphasis: not followed by whitespace, and
/// for `_` also not preceded by an alphanumeric character (so `snake_case`
/// never emphasizes). `run` is the run length; the run's own characters end at
/// `index + run`.
fn opens_emphasis(chars: &[char], index: usize, run: usize, delimiter: char) -> bool {
    let Some(&next) = chars.get(index + run) else {
        return false;
    };
    if next.is_whitespace() {
        return false;
    }
    if delimiter == '_' && index > 0 && chars[index - 1].is_alphanumeric() {
        return false;
    }
    true
}

/// Find the closing delimiter run: `min_run` or more of `delimiter`, preceded
/// by a non-whitespace character. Returns (content_end, past_closing_run).
fn closing_emphasis(
    chars: &[char],
    from: usize,
    delimiter: char,
    min_run: usize,
) -> Option<(usize, usize)> {
    let mut cursor = from;
    while cursor < chars.len() {
        if chars[cursor] == delimiter {
            let run = delimiter_run(chars, cursor, delimiter);
            let preceded_by_visible = cursor > from && !chars[cursor - 1].is_whitespace();
            if run >= min_run && preceded_by_visible {
                return Some((cursor, cursor + run));
            }
            cursor += run;
        } else {
            cursor += 1;
        }
    }
    None
}

/// Render inline spans to one display line. Attributes compose: each span
/// toggles only its own SGR attribute so bold containing code stays bold.
fn render_inline(spans: &[Span]) -> String {
    let mut out = String::new();
    render_spans(spans, &mut out);
    out
}

fn render_spans(spans: &[Span], out: &mut String) {
    for span in spans {
        match span {
            Span::Text(text) => out.push_str(text),
            Span::Bold(inner) => {
                out.push_str(sgr::BOLD);
                render_spans(inner, out);
                out.push_str(sgr::BOLD_OFF);
            }
            Span::Italic(inner) => {
                out.push_str(sgr::ITALIC);
                render_spans(inner, out);
                out.push_str(sgr::ITALIC_OFF);
            }
            Span::Code(text) => {
                out.push_str(sgr::CODE_FG);
                out.push_str(text);
                out.push_str(sgr::FG_DEFAULT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Visible text of one rendered line: SGR sequences removed.
    fn strip_sgr(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn test_fenced_code_block_keeps_body_whitespace_exact_and_fences_visible() {
        let source =
            "before\n```rust\nfn main() {\n    let deep =    1;\n\ttabbed();\n}\n```\nafter\n";
        let rendered = render_viewport_body(source);
        assert_eq!(
            rendered.len(),
            9,
            "one line per source line, fences included; got {rendered:?}"
        );
        assert_eq!(
            strip_sgr(&rendered[1]),
            "```rust",
            "opening fence stays text"
        );
        assert_eq!(
            strip_sgr(&rendered[2]),
            "fn main() {",
            "code body is byte-exact after SGR stripping"
        );
        assert_eq!(
            strip_sgr(&rendered[3]),
            "    let deep =    1;",
            "internal whitespace is exact"
        );
        assert_eq!(strip_sgr(&rendered[4]), "\ttabbed();", "tabs preserved");
        assert_eq!(strip_sgr(&rendered[6]), "```", "closing fence stays text");
        assert_eq!(strip_sgr(&rendered[7]), "after");
        assert!(
            rendered[2].contains(sgr::CODE_FG) || rendered[1].contains(sgr::DIM),
            "the block carries SGR styling in the viewport: {rendered:?}"
        );
    }

    #[test]
    fn test_bold_and_italic_drop_markers_and_style() {
        let rendered = render_viewport_body("plain **bold** and *soft* words\n");
        assert_eq!(
            rendered.len(),
            2,
            "one line per source line plus the trailing empty segment: {rendered:?}"
        );
        assert_eq!(
            strip_sgr(&rendered[0]),
            "plain bold and soft words",
            "markers are dropped in the rendered viewport: {rendered:?}"
        );
        assert!(
            rendered[0].contains(sgr::BOLD) && rendered[0].contains(sgr::ITALIC),
            "bold and italic carry SGR attributes: {rendered:?}"
        );
    }

    #[test]
    fn test_inline_code_is_tinted_and_marker_dropped() {
        let rendered = render_viewport_body("run `cargo test` now\n");
        assert_eq!(
            strip_sgr(&rendered[0]),
            "run cargo test now",
            "{rendered:?}"
        );
        assert!(rendered[0].contains(sgr::CODE_FG), "{rendered:?}");
    }

    #[test]
    fn test_unordered_lists_render_bullets_and_ordered_lists_keep_numbers() {
        let rendered = render_viewport_body("- first\n- second\n1. one\n2. two\n");
        assert_eq!(
            strip_sgr(&rendered[0]),
            "• first",
            "unordered dash becomes a bullet: {rendered:?}"
        );
        assert_eq!(strip_sgr(&rendered[1]), "• second");
        assert_eq!(
            strip_sgr(&rendered[2]),
            "1. one",
            "ordered numbering is already semantic and stays: {rendered:?}"
        );
        assert_eq!(strip_sgr(&rendered[3]), "2. two");
    }

    #[test]
    fn test_nested_list_indentation_is_preserved() {
        let rendered = render_viewport_body("- top\n  - nested\n");
        assert_eq!(strip_sgr(&rendered[0]), "• top");
        assert_eq!(
            strip_sgr(&rendered[1]),
            "  • nested",
            "the child item keeps its source indent: {rendered:?}"
        );
    }

    #[test]
    fn test_emphasis_inside_bold_nests_without_losing_either_attribute() {
        let rendered = render_viewport_body("**bold *soft* end**\n");
        let line = &rendered[0];
        assert_eq!(strip_sgr(line), "bold soft end", "{rendered:?}");
        assert!(
            line.contains(sgr::BOLD) && line.contains(sgr::ITALIC),
            "{rendered:?}"
        );
    }

    #[test]
    fn test_snake_case_and_arithmetic_do_not_emphasize() {
        let rendered = render_viewport_body("a_b_c and 2 * 3 * 4 stay literal\n");
        assert_eq!(
            rendered[0], "a_b_c and 2 * 3 * 4 stay literal",
            "no SGR and no marker loss: {rendered:?}"
        );
    }

    #[test]
    fn test_unmatched_delimiters_and_unterminated_fence_degrade_to_plain_text() {
        let rendered = render_viewport_body("dangling ** open * star\n");
        assert_eq!(rendered[0], "dangling ** open * star", "{rendered:?}");
        let rendered = render_viewport_body("```python\nprint('hi')\n");
        assert_eq!(
            rendered.len(),
            3,
            "an open fence renders as an open block, no bottom bookend: {rendered:?}"
        );
        assert_eq!(strip_sgr(&rendered[0]), "```python");
        assert_eq!(strip_sgr(&rendered[1]), "print('hi')");
    }

    #[test]
    fn test_malformed_and_foreign_constructs_pass_through_without_panic() {
        let source = "### Heading\n| a | b |\n[link](url)\n> quote\n";
        let rendered = render_viewport_body(source);
        assert_eq!(
            rendered.iter().map(|l| strip_sgr(l)).collect::<Vec<_>>(),
            vec![
                "### Heading".to_string(),
                "| a | b |".to_string(),
                "[link](url)".to_string(),
                "> quote".to_string(),
                String::new(),
            ],
            "outside the bounded subset everything is literal: {rendered:?}"
        );
    }

    #[test]
    fn test_tilde_fences_behave_like_backtick_fences() {
        let rendered = render_viewport_body("~~~\nraw ~~~\n~~~\n");
        assert_eq!(strip_sgr(&rendered[0]), "~~~");
        assert_eq!(
            strip_sgr(&rendered[1]),
            "raw ~~~",
            "a closing fence must be a bare run, so this stays interior"
        );
        assert_eq!(strip_sgr(&rendered[2]), "~~~");
    }

    #[test]
    fn test_tab_indented_fence_and_blank_lines_survive() {
        let rendered = render_viewport_body("a\n\n```\n  x\n\n  y\n```\n\nb\n");
        assert_eq!(
            strip_sgr(&rendered[3]),
            "  x",
            "blank and indented interior lines are kept verbatim: {rendered:?}"
        );
        assert_eq!(strip_sgr(&rendered[4]), "");
        assert_eq!(strip_sgr(&rendered[5]), "  y");
    }

    #[test]
    fn test_deep_nesting_hits_the_recursion_budget_and_stays_literal() {
        let source = "*".repeat(MAX_INLINE_DEPTH * 2 + 4);
        let rendered = render_viewport_body(&format!("{source}deep{source}\n"));
        assert_eq!(
            rendered.len(),
            2,
            "pathological emphasis must not recurse unbounded or panic: {rendered:?}"
        );
        assert!(
            strip_sgr(&rendered[0]).contains("deep"),
            "the payload survives the bounded parse: {rendered:?}"
        );
    }
}
