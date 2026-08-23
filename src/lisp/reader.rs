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

use super::types::Val;

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

fn tokenize(src: &str) -> Result<Vec<Tok>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
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

        match c {
            '$' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '$' {
                    i += 1;
                }
                if i >= chars.len() {
                    bail!("unterminated math expression: missing closing '$'");
                }
                let math_src: String = chars[start..i].iter().collect();
                i += 1; // consume closing '$'
                tokens.push(Tok::MathVal(parse_math(&math_src)?));
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
                let json_src: String = chars[start..i].iter().collect();
                let jv: serde_json::Value = serde_json::from_str(&json_src)
                    .map_err(|e| anyhow::anyhow!("JSON array literal: {e}"))?;
                tokens.push(Tok::JsonVal(crate::lisp::stdlib::json_val_to_lisp(jv)));
            }
            '{' => {
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
                let json_src: String = chars[start..i].iter().collect();
                let jv: serde_json::Value = serde_json::from_str(&json_src)
                    .map_err(|e| anyhow::anyhow!("JSON literal: {e}"))?;
                tokens.push(Tok::JsonVal(crate::lisp::stdlib::json_val_to_lisp(jv)));
            }
            '(' => {
                tokens.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Tok::RParen);
                i += 1;
            }
            '\'' => {
                tokens.push(Tok::Quote);
                i += 1;
            }
            '`' => {
                tokens.push(Tok::BackQuote);
                i += 1;
            }
            ',' => {
                if i + 1 < chars.len() && chars[i + 1] == '@' {
                    tokens.push(Tok::CommaAt);
                    i += 2;
                } else {
                    tokens.push(Tok::Comma);
                    i += 1;
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
                tokens.push(Tok::Str(s));
            }
            _ => {
                // Atom: read until delimiter
                let start = i;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch.is_whitespace()
                        || ch == '('
                        || ch == ')'
                        || ch == '"'
                        || ch == ';'
                        || ch == '\''
                        || ch == '`'
                        || ch == ','
                    {
                        break;
                    }
                    i += 1;
                }
                let atom: String = chars[start..i].iter().collect();
                // Lone "." is a special token
                if atom == "." {
                    tokens.push(Tok::Dot);
                } else {
                    tokens.push(Tok::Atom(atom));
                }
            }
        }
    }

    Ok(tokens)
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse all top-level expressions from `src`.
pub fn parse_str(src: &str) -> Result<Vec<Val>> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let mut exprs = Vec::new();
    while pos < tokens.len() {
        exprs.push(parse_one(&tokens, &mut pos)?);
    }
    Ok(exprs)
}

fn parse_one(tokens: &[Tok], pos: &mut usize) -> Result<Val> {
    if *pos >= tokens.len() {
        bail!("unexpected end of expression");
    }

    match &tokens[*pos] {
        Tok::LParen => {
            *pos += 1;
            let mut list = Vec::new();
            loop {
                if *pos >= tokens.len() {
                    bail!("missing closing ')'");
                }
                if tokens[*pos] == Tok::RParen {
                    *pos += 1;
                    return Ok(if list.is_empty() {
                        Val::Nil
                    } else {
                        Val::List(list)
                    });
                }
                list.push(parse_one(tokens, pos)?);
            }
        }

        Tok::RParen => bail!("unexpected ')'"),

        // 'x → (quote x)
        Tok::Quote => {
            *pos += 1;
            let inner = parse_one(tokens, pos)?;
            Ok(Val::List(vec![Val::Symbol("quote".to_string()), inner]))
        }

        // `x → (quasiquote x)
        Tok::BackQuote => {
            *pos += 1;
            let inner = parse_one(tokens, pos)?;
            Ok(Val::List(vec![
                Val::Symbol("quasiquote".to_string()),
                inner,
            ]))
        }

        // ,x → (unquote x)
        Tok::Comma => {
            *pos += 1;
            let inner = parse_one(tokens, pos)?;
            Ok(Val::List(vec![Val::Symbol("unquote".to_string()), inner]))
        }

        // ,@x → (unquote-splicing x)
        Tok::CommaAt => {
            *pos += 1;
            let inner = parse_one(tokens, pos)?;
            Ok(Val::List(vec![
                Val::Symbol("unquote-splicing".to_string()),
                inner,
            ]))
        }

        Tok::Dot => {
            *pos += 1;
            Ok(Val::Symbol(".".to_string()))
        }

        Tok::Str(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(Val::Str(s))
        }

        Tok::Atom(a) => {
            let a = a.clone();
            *pos += 1;
            parse_atom(&a)
        }

        Tok::MathVal(v) => {
            let v = v.clone();
            *pos += 1;
            Ok(v)
        }

        Tok::JsonVal(v) => {
            let v = v.clone();
            *pos += 1;
            Ok(v)
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
    fn test_parse_int() {
        assert_eq!(parse1("42"), Val::Int(42));
        assert_eq!(parse1("-7"), Val::Int(-7));
        assert_eq!(parse1("0xff"), Val::Int(255));
    }

    #[test]
    fn test_parse_float() {
        assert_eq!(parse1("3.14"), Val::Float(3.14));
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
