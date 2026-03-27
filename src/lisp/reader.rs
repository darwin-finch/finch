/// S-expression reader — tokeniser + recursive-descent parser.
///
/// Handles: atoms (symbols, ints, floats, booleans), string literals with
/// escape sequences, `'x` (quote shorthand), line comments (`;`), and
/// block comments (`#| … |#`).
use anyhow::{bail, Result};

use super::types::Val;

// ── Tokeniser ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Quote,        // '
    BackQuote,    // `  (quasiquote)
    Comma,        // ,  (unquote)
    CommaAt,      // ,@ (unquote-splicing)
    Dot,          // . (dotted pair, future use)
    Str(String),
    Atom(String),
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
            '(' => { tokens.push(Tok::LParen); i += 1; }
            ')' => { tokens.push(Tok::RParen); i += 1; }
            '\'' => { tokens.push(Tok::Quote); i += 1; }
            '`' => { tokens.push(Tok::BackQuote); i += 1; }
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
                                other => { s.push('\\'); s.push(other); }
                            }
                            i += 1;
                        }
                        '"' => { i += 1; break; }
                        ch => { s.push(ch); i += 1; }
                    }
                }
                tokens.push(Tok::Str(s));
            }
            _ => {
                // Atom: read until delimiter
                let start = i;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"'
                        || ch == ';' || ch == '\'' || ch == '`' || ch == ','
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
                    return Ok(if list.is_empty() { Val::Nil } else { Val::List(list) });
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
            Ok(Val::List(vec![Val::Symbol("quasiquote".to_string()), inner]))
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
            Ok(Val::List(vec![Val::Symbol("unquote-splicing".to_string()), inner]))
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
                Val::List(vec![
                    Val::Symbol("*".to_string()),
                    Val::Int(2),
                    Val::Int(3),
                ]),
            ])
        );
    }

    #[test]
    fn test_parse_quote_shorthand() {
        let v = parse1("'foo");
        assert_eq!(
            v,
            Val::List(vec![Val::Symbol("quote".to_string()), Val::Symbol("foo".to_string())])
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
}
