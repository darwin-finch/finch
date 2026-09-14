/// S-expression reader — tokeniser + recursive-descent parser.
///
/// Handles: atoms (symbols, ints, floats, booleans), string literals with
/// escape sequences, `'x` (quote shorthand), line comments (`;`), block
/// comments (`#| … |#`), and `$…$` math expressions.
///
/// Math expressions (`$…$`) are parsed into Lisp s-expressions:
///   `$x^2$`              → `(pow x 2)`
///   `$2*x + 1$`          → `(+ (* 2 x) 1)`
///   `$d/dx(x^2 + x)$`    → `(diff (+ (pow x 2) x) x)`
use anyhow::{bail, Result};
use std::ops::Range;

use super::types::Val;

/// A reader value paired with the exact byte range that produced it.
///
/// `children` follows the source tree for ordinary Lisp lists.  Reader
/// sugar that expands one token into several values (for example `'name`)
/// keeps the complete source span but deliberately has no invented child
/// ranges.  This lets typed lowering preserve a truthful enclosing origin
/// until it can attach explicit macro/reader expansion ancestry.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedVal {
    pub value: Val,
    pub span: Range<usize>,
    pub children: Vec<SpannedVal>,
}

// ── Math expression parser ─────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
enum MTok {
    Num(Val),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

fn tokenize_math(src: &str) -> Result<Vec<MTok>> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut tokens = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '+' => {
                tokens.push(MTok::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(MTok::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(MTok::Star);
                i += 1;
            }
            '/' => {
                tokens.push(MTok::Slash);
                i += 1;
            }
            '^' => {
                tokens.push(MTok::Caret);
                i += 1;
            }
            '(' => {
                tokens.push(MTok::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(MTok::RParen);
                i += 1;
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                if s.contains('.') {
                    tokens.push(MTok::Num(Val::Float(s.parse()?)));
                } else {
                    tokens.push(MTok::Num(Val::Int(s.parse()?)));
                }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                tokens.push(MTok::Ident(name));
            }
            _ => bail!("unexpected character in math expression: '{c}'"),
        }
    }
    Ok(tokens)
}

fn math_sym(s: &str) -> Val {
    Val::Symbol(s.to_string())
}
fn math_list(head: &str, args: Vec<Val>) -> Val {
    let mut v = vec![math_sym(head)];
    v.extend(args);
    Val::List(v)
}

/// Parse a full math expression from `src` into a Lisp Val tree.
pub fn parse_math(src: &str) -> Result<Val> {
    let tokens = tokenize_math(src)?;
    let mut pos = 0;
    let val = math_parse_add(&tokens, &mut pos)?;
    if pos < tokens.len() {
        bail!("unexpected token in math expression at position {pos}");
    }
    Ok(val)
}

fn math_parse_add(t: &[MTok], pos: &mut usize) -> Result<Val> {
    let mut lhs = math_parse_mul(t, pos)?;
    loop {
        match t.get(*pos) {
            Some(MTok::Plus) => {
                *pos += 1;
                lhs = math_list("+", vec![lhs, math_parse_mul(t, pos)?]);
            }
            Some(MTok::Minus) => {
                *pos += 1;
                lhs = math_list("-", vec![lhs, math_parse_mul(t, pos)?]);
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn math_parse_mul(t: &[MTok], pos: &mut usize) -> Result<Val> {
    let mut lhs = math_parse_pow(t, pos)?;
    loop {
        match t.get(*pos) {
            Some(MTok::Star) => {
                *pos += 1;
                lhs = math_list("*", vec![lhs, math_parse_pow(t, pos)?]);
            }
            Some(MTok::Slash) => {
                *pos += 1;
                lhs = math_list("/", vec![lhs, math_parse_pow(t, pos)?]);
            }
            // Implicit multiplication: number or ident immediately follows
            Some(MTok::Num(_)) | Some(MTok::Ident(_)) | Some(MTok::LParen) => {
                lhs = math_list("*", vec![lhs, math_parse_pow(t, pos)?]);
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn math_parse_pow(t: &[MTok], pos: &mut usize) -> Result<Val> {
    let lhs = math_parse_unary(t, pos)?;
    if t.get(*pos) == Some(&MTok::Caret) {
        *pos += 1;
        let rhs = math_parse_pow(t, pos)?; // right-associative
        Ok(math_list("pow", vec![lhs, rhs]))
    } else {
        Ok(lhs)
    }
}

fn math_parse_unary(t: &[MTok], pos: &mut usize) -> Result<Val> {
    if t.get(*pos) == Some(&MTok::Minus) {
        *pos += 1;
        Ok(math_list("neg", vec![math_parse_atom(t, pos)?]))
    } else {
        math_parse_atom(t, pos)
    }
}

fn math_parse_atom(t: &[MTok], pos: &mut usize) -> Result<Val> {
    // d/d<var>(<expr>) — differential operator
    if let Some(MTok::Ident(name)) = t.get(*pos) {
        if name == "d"
            && t.get(*pos + 1) == Some(&MTok::Slash)
            && matches!(t.get(*pos + 2), Some(MTok::Ident(s)) if s.starts_with('d') && s.len() > 1)
        {
            let var_name = match &t[*pos + 2] {
                MTok::Ident(s) => s[1..].to_string(), // strip leading 'd'
                _ => unreachable!(),
            };
            *pos += 3;
            if t.get(*pos) != Some(&MTok::LParen) {
                bail!("d/d{var_name}: expected '(' after differential operator");
            }
            *pos += 1;
            let expr = math_parse_add(t, pos)?;
            if t.get(*pos) != Some(&MTok::RParen) {
                bail!("d/d{var_name}: missing ')'");
            }
            *pos += 1;
            return Ok(math_list("diff", vec![expr, math_sym(&var_name)]));
        }
    }

    match t.get(*pos) {
        Some(MTok::Num(v)) => {
            let v = v.clone();
            *pos += 1;
            Ok(v)
        }
        Some(MTok::Ident(name)) => {
            let name = name.clone();
            *pos += 1;
            // Function call: name(arg)
            if t.get(*pos) == Some(&MTok::LParen) {
                *pos += 1;
                let arg = math_parse_add(t, pos)?;
                if t.get(*pos) != Some(&MTok::RParen) {
                    bail!("missing ')' in function call '{name}'");
                }
                *pos += 1;
                Ok(math_list(&name, vec![arg]))
            } else {
                Ok(Val::Symbol(name))
            }
        }
        Some(MTok::LParen) => {
            *pos += 1;
            let inner = math_parse_add(t, pos)?;
            if t.get(*pos) != Some(&MTok::RParen) {
                bail!("missing ')' in math expression");
            }
            *pos += 1;
            Ok(inner)
        }
        other => bail!("unexpected token in math expression: {other:?}"),
    }
}

// ── Tokeniser ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    RBracket,
    /// Typed Finch record literal: `{ :field value ... }`. JSON object
    /// literals remain a distinct reader form and are recognized when the
    /// opening brace is followed by a JSON key/value spelling.
    LBrace,
    RBrace,
    Quote,     // '
    BackQuote, // `  (quasiquote)
    Comma,     // ,  (unquote)
    CommaAt,   // ,@ (unquote-splicing)
    Dot,       // . (dotted pair, future use)
    Str(String),
    Atom(String),
    MathVal(Val), // from $...$ math expression
    JsonVal(Val), // from {...} JSON literal
}

#[derive(Debug, PartialEq)]
struct SpannedTok {
    kind: Tok,
    span: Range<usize>,
}

/// Convert pasted JSON literals into the neutral Lisp syntax tree consumed by
/// the typed frontend. This is reader behavior, not a native evaluator builtin.
fn json_value_to_syntax(value: serde_json::Value) -> Val {
    match value {
        serde_json::Value::Null => Val::Nil,
        serde_json::Value::Bool(value) => Val::Bool(value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(Val::Int)
            .unwrap_or_else(|| Val::Float(value.as_f64().unwrap_or(f64::NAN))),
        serde_json::Value::String(value) => Val::Str(value),
        serde_json::Value::Array(values) => {
            Val::List(values.into_iter().map(json_value_to_syntax).collect())
        }
        serde_json::Value::Object(values) => Val::List(
            values
                .into_iter()
                .map(|(key, value)| Val::List(vec![Val::Str(key), json_value_to_syntax(value)]))
                .collect(),
        ),
    }
}

fn tokenize(src: &str) -> Result<Vec<SpannedTok>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let byte_offsets: Vec<usize> = src
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(src.len()))
        .collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Whitespace
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Line comment
        if c == ';' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Block comment #| … |#
        if c == '#' && i + 1 < chars.len() && chars[i + 1] == '|' {
            i += 2;
            loop {
                if i + 1 >= chars.len() {
                    bail!("unterminated block comment");
                }
                if chars[i] == '|' && chars[i + 1] == '#' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        let token_start = i;

        // Compact structural types are annotation atoms, not record/JSON
        // values. Keeping the balanced spelling intact lets typed Lisp and
        // Co-Forth share one type-expression grammar.
        if let Some(end) = compact_braced_type_end(&chars, i) {
            tokens.push(SpannedTok {
                kind: Tok::Atom(chars[i..end].iter().collect()),
                span: byte_offsets[i]..byte_offsets[end],
            });
            i = end;
            continue;
        }

        let kind = match c {
            '$' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '$' {
                    i += 1;
                }
                if i >= chars.len() {
                    bail!("unterminated math expression: missing closing '$'");
                }
                let math_src = &src[byte_offsets[start]..byte_offsets[i]];
                i += 1; // consume closing '$'
                Tok::MathVal(parse_math(math_src)?)
            }
            '[' => {
                // JSON array literal — bracket-balanced span, parsed with serde_json.
                let start = i;
                let mut depth = 0usize;
                let mut in_str = false;
                let mut escape = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if escape {
                        escape = false;
                        i += 1;
                        continue;
                    }
                    if ch == '\\' && in_str {
                        escape = true;
                        i += 1;
                        continue;
                    }
                    if ch == '"' {
                        in_str = !in_str;
                        i += 1;
                        continue;
                    }
                    if !in_str {
                        if ch == '[' {
                            depth += 1;
                        } else if ch == ']' {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                    }
                    i += 1;
                }
                let json_src = &src[byte_offsets[start]..byte_offsets[i]];
                let jv: serde_json::Value = serde_json::from_str(json_src)
                    .map_err(|e| anyhow::anyhow!("JSON array literal: {e}"))?;
                Tok::JsonVal(json_value_to_syntax(jv))
            }
            '{' => {
                if brace_starts_typed_record(&chars, i) {
                    i += 1;
                    tokens.push(SpannedTok {
                        kind: Tok::LBrace,
                        span: byte_offsets[token_start]..byte_offsets[i],
                    });
                    continue;
                }
                // JSON literal — read a brace-balanced span, then parse with serde_json.
                let start = i;
                let mut depth = 0usize;
                let mut in_str = false;
                let mut escape = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if escape {
                        escape = false;
                        i += 1;
                        continue;
                    }
                    if ch == '\\' && in_str {
                        escape = true;
                        i += 1;
                        continue;
                    }
                    if ch == '"' {
                        in_str = !in_str;
                        i += 1;
                        continue;
                    }
                    if !in_str {
                        if ch == '{' {
                            depth += 1;
                        } else if ch == '}' {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                    }
                    i += 1;
                }
                let json_src = &src[byte_offsets[start]..byte_offsets[i]];
                let jv: serde_json::Value = serde_json::from_str(json_src)
                    .map_err(|e| anyhow::anyhow!("JSON literal: {e}"))?;
                Tok::JsonVal(json_value_to_syntax(jv))
            }
            ']' => {
                i += 1;
                Tok::RBracket
            }
            '}' => {
                i += 1;
                Tok::RBrace
            }
            '(' => {
                i += 1;
                Tok::LParen
            }
            ')' => {
                i += 1;
                Tok::RParen
            }
            '\'' => {
                i += 1;
                Tok::Quote
            }
            '`' => {
                i += 1;
                Tok::BackQuote
            }
            ',' => {
                if i + 1 < chars.len() && chars[i + 1] == '@' {
                    i += 2;
                    Tok::CommaAt
                } else {
                    i += 1;
                    Tok::Comma
                }
            }
            '"' => {
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= chars.len() {
                        bail!("unterminated string literal");
                    }
                    match chars[i] {
                        '\\' => {
                            i += 1;
                            if i >= chars.len() {
                                bail!("unterminated escape sequence");
                            }
                            match chars[i] {
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                'r' => s.push('\r'),
                                '"' => s.push('"'),
                                '\\' => s.push('\\'),
                                '0' => s.push('\0'),
                                other => {
                                    s.push('\\');
                                    s.push(other);
                                }
                            }
                            i += 1;
                        }
                        '"' => {
                            i += 1;
                            break;
                        }
                        ch => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                Tok::Str(s)
            }
            _ => {
                // Atom: read until delimiter
                let start = i;
                let mut angle_depth = 0usize;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '<' {
                        angle_depth += 1;
                        i += 1;
                        continue;
                    }
                    if ch == '>' && angle_depth > 0 {
                        angle_depth -= 1;
                        i += 1;
                        continue;
                    }
                    if ch.is_whitespace()
                        || ch == '('
                        || ch == ')'
                        || ch == '{'
                        || ch == '}'
                        || ch == '"'
                        || ch == ';'
                        || ch == '\''
                        || ch == '`'
                        || (ch == ',' && angle_depth == 0)
                    {
                        break;
                    }
                    i += 1;
                }
                let atom: String = chars[start..i].iter().collect();
                // Lone "." is a special token
                if atom == "." {
                    Tok::Dot
                } else {
                    Tok::Atom(atom)
                }
            }
        };
        tokens.push(SpannedTok {
            kind,
            span: byte_offsets[token_start]..byte_offsets[i],
        });
    }

    Ok(tokens)
}

fn compact_braced_type_end(chars: &[char], start: usize) -> Option<usize> {
    let has_type_prefix = ["record{", "variant{"].iter().any(|prefix| {
        let prefix: Vec<char> = prefix.chars().collect();
        chars.get(start..start + prefix.len()) == Some(prefix.as_slice())
    });
    if !has_type_prefix {
        return None;
    }
    let mut depth = 0usize;
    for (offset, character) in chars[start..].iter().enumerate() {
        if character.is_whitespace() || *character == '"' {
            return None;
        }
        match character {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// `{ :name value }` is Finch's typed record literal. Preserve ordinary JSON
/// objects (`{"name": value}`) as the existing explicit JSON reader form;
/// the first non-whitespace character makes the two spellings unambiguous.
fn brace_starts_typed_record(chars: &[char], open: usize) -> bool {
    chars
        .get(open + 1..)
        .and_then(|tail| tail.iter().find(|character| !character.is_whitespace()))
        .is_some_and(|character| *character == ':')
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse all top-level expressions from `src`.
pub fn parse_str(src: &str) -> Result<Vec<Val>> {
    Ok(parse_str_spanned(src)?
        .into_iter()
        .map(|form| form.value)
        .collect())
}

/// Parse all top-level expressions while retaining their source structure.
///
/// The reader constructs values and byte ranges together from one token stream.
/// Reader sugar keeps its exact enclosing span without inventing child ranges.
pub fn parse_str_spanned(src: &str) -> Result<Vec<SpannedVal>> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let mut exprs = Vec::new();
    while pos < tokens.len() {
        exprs.push(parse_one(&tokens, &mut pos)?);
    }
    Ok(exprs)
}

impl SpannedVal {
    fn leaf(value: Val, span: Range<usize>) -> Self {
        Self {
            value,
            span,
            children: Vec::new(),
        }
    }

    fn list(children: Vec<Self>, span: Range<usize>) -> Self {
        Self {
            value: Val::List(children.iter().map(|child| child.value.clone()).collect()),
            span,
            children,
        }
    }
}

fn parse_one(tokens: &[SpannedTok], pos: &mut usize) -> Result<SpannedVal> {
    let Some(token) = tokens.get(*pos) else {
        bail!("unexpected end of expression");
    };
    let start = token.span.start;

    match &token.kind {
        Tok::LParen => {
            *pos += 1;
            let mut children = Vec::new();
            loop {
                let Some(next) = tokens.get(*pos) else {
                    bail!("missing closing ')'");
                };
                if next.kind == Tok::RParen {
                    *pos += 1;
                    let span = start..next.span.end;
                    return Ok(if children.is_empty() {
                        SpannedVal::leaf(Val::Nil, span)
                    } else {
                        SpannedVal::list(children, span)
                    });
                }
                children.push(parse_one(tokens, pos)?);
            }
        }

        Tok::RParen => bail!("unexpected ')'"),
        Tok::RBracket => bail!("unexpected ']'"),

        Tok::LBrace => {
            *pos += 1;
            let mut fields = Vec::new();
            loop {
                let Some(next) = tokens.get(*pos) else {
                    bail!("missing closing '}}' for typed record");
                };
                if next.kind == Tok::RBrace {
                    *pos += 1;
                    let span = start..next.span.end;
                    // The hidden marker uses the enclosing record origin; field
                    // names and values retain their actual source ranges.
                    fields.insert(
                        0,
                        SpannedVal::leaf(
                            Val::Symbol("finch-record-literal".to_string()),
                            span.clone(),
                        ),
                    );
                    return Ok(SpannedVal::list(fields, span));
                }
                let Tok::Atom(field) = &next.kind else {
                    bail!("typed record fields must use :name value syntax");
                };
                let Some(name) = field.strip_prefix(':') else {
                    bail!("typed record fields must use :name value syntax");
                };
                if name.is_empty() {
                    bail!("typed record field name cannot be empty");
                }
                let name = name.to_owned();
                let name_span = next.span.clone();
                *pos += 1;
                if *pos >= tokens.len() || tokens[*pos].kind == Tok::RBrace {
                    bail!("typed record field ':{name}' needs a value");
                }
                let value = parse_one(tokens, pos)?;
                let span = name_span.start..value.span.end;
                fields.push(SpannedVal::list(
                    vec![SpannedVal::leaf(Val::Symbol(name), name_span), value],
                    span,
                ));
            }
        }

        Tok::RBrace => bail!("unexpected '}}'"),

        Tok::Quote | Tok::BackQuote | Tok::Comma | Tok::CommaAt => {
            let head = match token.kind {
                Tok::Quote => "quote",
                Tok::BackQuote => "quasiquote",
                Tok::Comma => "unquote",
                Tok::CommaAt => "unquote-splicing",
                _ => unreachable!(),
            };
            *pos += 1;
            let inner = parse_one(tokens, pos)?;
            Ok(SpannedVal::leaf(
                Val::List(vec![Val::Symbol(head.to_string()), inner.value]),
                start..inner.span.end,
            ))
        }

        Tok::Dot | Tok::Str(_) | Tok::Atom(_) | Tok::MathVal(_) | Tok::JsonVal(_) => {
            let value = match &token.kind {
                Tok::Dot => Val::Symbol(".".to_string()),
                Tok::Str(s) => Val::Str(s.clone()),
                Tok::Atom(a) => parse_atom(a)?,
                Tok::MathVal(value) | Tok::JsonVal(value) => value.clone(),
                _ => unreachable!(),
            };
            *pos += 1;
            Ok(SpannedVal::leaf(value, token.span.clone()))
        }
    }
}

fn parse_atom(a: &str) -> Result<Val> {
    // Boolean literals
    match a {
        "#t" | "true" => return Ok(Val::Bool(true)),
        "#f" | "false" => return Ok(Val::Bool(false)),
        "()" | "nil" => return Ok(Val::Nil),
        _ => {}
    }

    // Hex literal: 0x...
    if let Some(hex) = a.strip_prefix("0x").or_else(|| a.strip_prefix("0X")) {
        if let Ok(n) = i64::from_str_radix(hex, 16) {
            return Ok(Val::Int(n));
        }
    }

    // Integer
    if let Ok(n) = a.parse::<i64>() {
        return Ok(Val::Int(n));
    }

    // Float
    if let Ok(f) = a.parse::<f64>() {
        return Ok(Val::Float(f));
    }

    // Symbol
    Ok(Val::Symbol(a.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse1(s: &str) -> Val {
        parse_str(s).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn test_parse_projections_reject_unmatched_array_closer() {
        for source in ["]", "(] 1)", "' ]", "{ :field ] }"] {
            let plain = parse_str(source);
            let spanned = parse_str_spanned(source);
            assert!(
                plain.is_err() && spanned.is_err(),
                "both reader projections must reject an unmatched array closer: source={source:?}, plain={plain:?}, spanned={spanned:?}"
            );
            assert_eq!(
                plain.unwrap_err().to_string(),
                spanned.unwrap_err().to_string(),
                "both reader projections must report the same malformed delimiter: source={source:?}"
            );
        }
    }

    #[test]
    fn test_spanned_reader_preserves_unicode_record_and_nested_list_origins() {
        let source = "; λ\n({ :名 (list \"é\" ()) :型 record{名:string} } #| 文 |# 42)";
        let forms = parse_str_spanned(source).expect("Unicode nested record parses");
        let root = &forms[0];
        let record = &root.children[0];
        let field = &record.children[1];
        let value = &field.children[1];
        for (node, spelling) in [
            (
                root,
                "({ :名 (list \"é\" ()) :型 record{名:string} } #| 文 |# 42)",
            ),
            (record, "{ :名 (list \"é\" ()) :型 record{名:string} }"),
            (
                &record.children[0],
                "{ :名 (list \"é\" ()) :型 record{名:string} }",
            ),
            (field, ":名 (list \"é\" ())"),
            (&field.children[0], ":名"),
            (value, "(list \"é\" ())"),
            (&value.children[1], "\"é\""),
            (&value.children[2], "()"),
            (&record.children[2].children[1], "record{名:string}"),
            (&root.children[1], "42"),
        ] {
            assert_eq!(
                source.get(node.span.clone()),
                Some(spelling),
                "reader origins must be exact UTF-8 byte ranges into the source: node={node:?}, source={source:?}"
            );
        }
        fn assert_children_match_values(node: &SpannedVal) {
            if node.children.is_empty() {
                return;
            }
            let values: Vec<_> = node
                .children
                .iter()
                .map(|child| child.value.clone())
                .collect();
            assert_eq!(
                node.value,
                Val::List(values),
                "a structured reader node must project exactly to its child values: node={node:?}"
            );
            for child in &node.children {
                assert_children_match_values(child);
            }
        }
        assert_children_match_values(root);
        assert_eq!(
            parse_str(source).unwrap(),
            vec![root.value.clone()],
            "plain parsing must project the authoritative spanned tree: source={source:?}, root={root:?}"
        );
    }

    #[test]
    fn test_spanned_reader_sugar_uses_complete_original_ranges() {
        let spellings = [
            "' #| λ |# (a 'b)",
            "`(a ,b ,@c)",
            ", #| λ |# x",
            ",@ (a b)",
            "$2*x + 1$",
            r#"["λ]\"", {"x": [1]}]"#,
            r#"{"x": ["}\\", 1]}"#,
        ];
        for spelling in spellings {
            let source = format!("; é\n(list {spelling} 42)");
            let forms = parse_str_spanned(&source).unwrap_or_else(|error| {
                panic!("reader sugar must parse inside an ordinary list: source={source:?}, error={error}")
            });
            let sugar = &forms[0].children[1];
            assert_eq!(
                source.get(sugar.span.clone()), Some(spelling),
                "sugar must retain its complete original UTF-8 range: source={source:?}, sugar={sugar:?}"
            );
            assert!(
                sugar.children.is_empty(),
                "reader expansion must not invent child origins: source={source:?}, sugar={sugar:?}"
            );
            assert_eq!(
                source.get(forms[0].children[2].span.clone()),
                Some("42"),
                "sugar must not consume the following form: source={source:?}, forms={forms:?}"
            );
        }
    }

    #[test]
    fn test_parse_projections_share_malformed_form_diagnostics() {
        for source in [
            "(",
            ")",
            "}",
            "(}",
            "'",
            "' )",
            ",@",
            "$1",
            "[1",
            "[1}",
            "{\"x\": 1",
            "{ :x 1",
            "{ :x }",
            "\"abc",
            "#| comment",
        ] {
            let plain = parse_str(source).expect_err("malformed form must fail plain parsing");
            let spanned =
                parse_str_spanned(source).expect_err("malformed form must fail spanned parsing");
            assert_eq!(
                plain.to_string(), spanned.to_string(),
                "both projections must share grammar errors: source={source:?}, plain={plain:?}, spanned={spanned:?}"
            );
        }
    }

    #[test]
    fn test_parse_nil() {
        assert_eq!(parse1("()"), Val::Nil);
        assert_eq!(parse1("nil"), Val::Nil);
    }

    #[test]
    fn test_parse_bool() {
        assert_eq!(parse1("#t"), Val::Bool(true));
        assert_eq!(parse1("#f"), Val::Bool(false));
    }

    #[test]
    fn typed_brace_records_remain_distinct_from_json_objects() {
        assert_eq!(
            parse1("{ :name \"Ada\" :age 37 }"),
            Val::List(vec![
                Val::Symbol("finch-record-literal".into()),
                Val::List(vec![Val::Symbol("name".into()), Val::Str("Ada".into())]),
                Val::List(vec![Val::Symbol("age".into()), Val::Int(37)]),
            ])
        );
        // Existing JSON object spelling deliberately remains a reader value;
        // a typed program must still cross the explicit json-* boundary.
        assert_eq!(
            parse1("{\"name\": \"Ada\"}"),
            json_value_to_syntax(serde_json::json!({"name": "Ada"}))
        );
        assert_eq!(parse1("{}"), json_value_to_syntax(serde_json::json!({})));
        let spanned = parse_str_spanned("{ :name \"Ada\" :age 37 }").unwrap();
        assert_eq!(spanned[0].children.len(), 3);
        assert_eq!(spanned[0].children[1].children.len(), 2);
    }

    #[test]
    fn test_parse_int() {
        assert_eq!(parse1("42"), Val::Int(42));
        assert_eq!(parse1("-7"), Val::Int(-7));
        assert_eq!(parse1("0xff"), Val::Int(255));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_parse_float() {
        assert_eq!(parse1("3.14"), Val::Float(3.14));
    }

    #[test]
    fn keeps_comma_separated_generic_types_as_one_atom() {
        assert_eq!(
            parse1("result<option<int>,string>"),
            Val::Symbol("result<option<int>,string>".into())
        );
        let spanned = parse_str_spanned("(define (example) : result<int,string> (ok 1))")
            .expect("generic type annotation parses");
        assert_eq!(
            spanned[0].children[3].value,
            Val::Symbol("result<int,string>".into())
        );
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(parse1(r#""hello""#), Val::Str("hello".to_string()));
        assert_eq!(parse1(r#""a\nb""#), Val::Str("a\nb".to_string()));
    }

    #[test]
    fn test_parse_symbol() {
        assert_eq!(parse1("foo"), Val::Symbol("foo".to_string()));
        assert_eq!(parse1("+"), Val::Symbol("+".to_string()));
    }

    #[test]
    fn test_parse_list() {
        let v = parse1("(1 2 3)");
        assert_eq!(v, Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]));
    }

    #[test]
    fn test_parse_nested() {
        let v = parse1("(+ 1 (* 2 3))");
        assert_eq!(
            v,
            Val::List(vec![
                Val::Symbol("+".to_string()),
                Val::Int(1),
                Val::List(vec![Val::Symbol("*".to_string()), Val::Int(2), Val::Int(3),]),
            ])
        );
    }

    #[test]
    fn spanned_reader_retains_exact_nested_list_and_token_ranges() {
        let source = "; lead\n(+ 1 (* 2 3))";
        let forms = parse_str_spanned(source).unwrap();
        let root = &forms[0];
        assert_eq!(&source[root.span.clone()], "(+ 1 (* 2 3))");
        assert_eq!(&source[root.children[0].span.clone()], "+");
        assert_eq!(&source[root.children[1].span.clone()], "1");
        let product = &root.children[2];
        assert_eq!(&source[product.span.clone()], "(* 2 3)");
        assert_eq!(&source[product.children[2].span.clone()], "3");
    }

    #[test]
    fn spanned_reader_keeps_reader_sugar_truthful_without_invented_children() {
        let source = "'answer $2*x$ {\"answer\": 42}";
        let forms = parse_str_spanned(source).unwrap();
        assert_eq!(&source[forms[0].span.clone()], "'answer");
        assert!(forms[0].children.is_empty());
        assert_eq!(&source[forms[1].span.clone()], "$2*x$");
        assert!(forms[1].children.is_empty());
        assert_eq!(&source[forms[2].span.clone()], "{\"answer\": 42}");
        assert!(forms[2].children.is_empty());
    }

    #[test]
    fn test_parse_quote_shorthand() {
        let v = parse1("'foo");
        assert_eq!(
            v,
            Val::List(vec![
                Val::Symbol("quote".to_string()),
                Val::Symbol("foo".to_string())
            ])
        );
    }

    #[test]
    fn test_parse_line_comment() {
        let exprs = parse_str("; this is ignored\n42").unwrap();
        assert_eq!(exprs, vec![Val::Int(42)]);
    }

    #[test]
    fn test_parse_multiple_exprs() {
        let exprs = parse_str("1 2 3").unwrap();
        assert_eq!(exprs, vec![Val::Int(1), Val::Int(2), Val::Int(3)]);
    }

    #[test]
    fn compact_structural_types_are_single_symbols() {
        let source = "record{name:string,meta:map<string,list<int>>} \
                      variant{none|some(int)|metadata(record{name:string})}";
        let exprs = parse_str(source).unwrap();
        assert_eq!(
            exprs,
            vec![
                Val::Symbol("record{name:string,meta:map<string,list<int>>}".into()),
                Val::Symbol("variant{none|some(int)|metadata(record{name:string})}".into()),
            ]
        );

        let spanned = parse_str_spanned(source).unwrap();
        assert_eq!(
            &source[spanned[0].span.clone()],
            "record{name:string,meta:map<string,list<int>>}"
        );
        assert_eq!(
            &source[spanned[1].span.clone()],
            "variant{none|some(int)|metadata(record{name:string})}"
        );
    }

    #[test]
    fn test_parse_block_comment() {
        let exprs = parse_str("#| ignored |# 99").unwrap();
        assert_eq!(exprs, vec![Val::Int(99)]);
    }

    // ── Math reader tests ─────────────────────────────────────────────────────

    fn math(s: &str) -> Val {
        parse_math(s).unwrap()
    }

    #[test]
    fn test_math_pow() {
        assert_eq!(
            math("x^2"),
            Val::List(vec![
                Val::Symbol("pow".into()),
                Val::Symbol("x".into()),
                Val::Int(2)
            ])
        );
    }

    #[test]
    fn test_math_add_mul() {
        // 2*x + 1  →  (+ (* 2 x) 1)
        let e = math("2*x + 1");
        assert_eq!(
            e,
            Val::List(vec![
                Val::Symbol("+".into()),
                Val::List(vec![
                    Val::Symbol("*".into()),
                    Val::Int(2),
                    Val::Symbol("x".into())
                ]),
                Val::Int(1),
            ])
        );
    }

    #[test]
    fn test_math_implicit_mul() {
        // 3x  →  (* 3 x)
        let e = math("3x");
        assert_eq!(
            e,
            Val::List(vec![
                Val::Symbol("*".into()),
                Val::Int(3),
                Val::Symbol("x".into())
            ])
        );
    }

    #[test]
    fn test_math_diff_operator() {
        // d/dx(x^2)  →  (diff (pow x 2) x)
        let e = math("d/dx(x^2)");
        assert_eq!(
            e,
            Val::List(vec![
                Val::Symbol("diff".into()),
                Val::List(vec![
                    Val::Symbol("pow".into()),
                    Val::Symbol("x".into()),
                    Val::Int(2)
                ]),
                Val::Symbol("x".into()),
            ])
        );
    }

    #[test]
    fn test_math_dollar_in_lisp() {
        // $x^2$ in Lisp source → (pow x 2)
        let exprs = parse_str("$x^2$").unwrap();
        assert_eq!(
            exprs,
            vec![Val::List(vec![
                Val::Symbol("pow".into()),
                Val::Symbol("x".into()),
                Val::Int(2),
            ])]
        );
    }

    // ── JSON literal syntax ───────────────────────────────────────────────────

    #[test]
    fn test_json_brace_object() {
        let exprs = parse_str(r#"{"name": "alice", "age": 30}"#).unwrap();
        assert_eq!(exprs.len(), 1);
        // Should be an alist: (("name" "alice") ("age" 30)) — order depends on serde_json
        let Val::List(pairs) = &exprs[0] else {
            panic!("expected list, got {:?}", exprs[0])
        };
        assert_eq!(pairs.len(), 2);
        let find = |key: &str| {
            pairs
                .iter()
                .any(|p| matches!(p, Val::List(kv) if kv[0] == Val::Str(key.into())))
        };
        assert!(find("name"), "key 'name' not found in {exprs:?}");
        assert!(find("age"), "key 'age' not found in {exprs:?}");
    }

    #[test]
    fn test_json_array_literal() {
        let exprs = parse_str("[1, 2, 3]").unwrap();
        assert_eq!(
            exprs,
            vec![Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)])]
        );
    }

    #[test]
    fn test_json_bool_and_null() {
        let exprs = parse_str(r#"{"ok": true, "missing": null}"#).unwrap();
        let Val::List(pairs) = &exprs[0] else {
            panic!("expected list")
        };
        let find_val = |key: &str| {
            pairs.iter().find_map(|p| {
                if let Val::List(kv) = p {
                    if kv[0] == Val::Str(key.into()) {
                        return Some(kv[1].clone());
                    }
                }
                None
            })
        };
        assert_eq!(find_val("ok"), Some(Val::Bool(true)));
        assert_eq!(find_val("missing"), Some(Val::Nil));
    }

    #[test]
    fn test_json_in_lisp_expression() {
        // JSON literal as an argument to a Lisp call
        let exprs = parse_str(r#"(car {"x": 1})"#).unwrap();
        assert_eq!(exprs.len(), 1);
        if let Val::List(items) = &exprs[0] {
            assert_eq!(items[0], Val::Symbol("car".into()));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn test_json_nested_object() {
        let exprs = parse_str(r#"{"a": {"b": 2}}"#).unwrap();
        if let Val::List(outer) = &exprs[0] {
            if let Val::List(pair) = &outer[0] {
                assert_eq!(pair[0], Val::Str("a".into()));
                assert!(matches!(&pair[1], Val::List(_))); // nested alist
            }
        }
    }

    #[test]
    fn test_json_invalid_returns_err() {
        assert!(parse_str(r#"{"bad: json}"#).is_err());
    }
}
