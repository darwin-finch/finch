//! Packrat matcher for the published Finch wire GBNF.
//!
//! This interprets the generated grammar; it is not a second language
//! definition. Semantic safety stays with the compiler and verifier.

use std::collections::HashMap;

#[derive(Debug, Clone)]
enum Expr {
    Any,
    Literal(String),
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Seq(Vec<Expr>),
    Alt(Vec<Expr>),
    Star(Box<Expr>),
    Plus(Box<Expr>),
    Opt(Box<Expr>),
    Rule(usize),
}

#[derive(Debug, Clone, Copy)]
enum ClassItem {
    Char(char),
    Range(char, char),
}

#[derive(Debug, Clone)]
pub struct Grammar {
    rules: Vec<Expr>,
    root: usize,
}

#[derive(Debug)]
pub struct GbnfError {
    pub message: String,
}

impl std::fmt::Display for GbnfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Parse a GBNF document into a matchable grammar.
pub fn parse(source: &str) -> Result<Grammar, GbnfError> {
    let mut parser = Parser {
        chars: source.chars().collect(),
        pos: 0,
    };
    let mut names = Vec::new();
    let mut bodies = Vec::new();
    loop {
        parser.skip();
        if parser.pos >= parser.chars.len() {
            break;
        }
        let name = parser
            .ident()
            .ok_or_else(|| parser.error("expected rule name"))?;
        parser.skip();
        if !parser.eat_str("::=") {
            return Err(parser.error("expected ::= after rule name"));
        }
        parser.skip();
        let expr = parser.expr()?;
        if let Some(index) = names.iter().position(|existing| existing == &name) {
            bodies[index] = expr;
        } else {
            names.push(name);
            bodies.push(expr);
        }
    }
    let root = names
        .iter()
        .position(|name| name == "root")
        .ok_or_else(|| GbnfError {
            message: "GBNF is missing a root rule".to_string(),
        })?;
    let mut rules = Vec::with_capacity(bodies.len());
    for body in bodies {
        rules.push(resolve_names(body, &names)?);
    }
    Ok(Grammar { rules, root })
}

/// Named references are stored as `Expr::Literal` with a leading NUL during parse.
fn resolve_names(expr: Expr, names: &[String]) -> Result<Expr, GbnfError> {
    match expr {
        Expr::Literal(text) if text.starts_with('\0') => {
            let name = &text[1..];
            let index = names
                .iter()
                .position(|existing| existing == name)
                .ok_or_else(|| GbnfError {
                    message: format!("GBNF refers to unknown rule {name}"),
                })?;
            Ok(Expr::Rule(index))
        }
        Expr::Seq(items) => Ok(Expr::Seq(
            items
                .into_iter()
                .map(|item| resolve_names(item, names))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Alt(items) => Ok(Expr::Alt(
            items
                .into_iter()
                .map(|item| resolve_names(item, names))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Star(inner) => Ok(Expr::Star(Box::new(resolve_names(*inner, names)?))),
        Expr::Plus(inner) => Ok(Expr::Plus(Box::new(resolve_names(*inner, names)?))),
        Expr::Opt(inner) => Ok(Expr::Opt(Box::new(resolve_names(*inner, names)?))),
        other => Ok(other),
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn error(&self, message: &str) -> GbnfError {
        GbnfError {
            message: format!("{message} at GBNF character {}", self.pos),
        }
    }

    fn skip(&mut self) {
        loop {
            while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
                self.pos += 1;
            }
            if self.pos < self.chars.len() && self.chars[self.pos] == '#' {
                while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        Some(ch)
    }

    fn eat_str(&mut self, needle: &str) -> bool {
        let start = self.pos;
        for expected in needle.chars() {
            if self.bump() != Some(expected) {
                self.pos = start;
                return false;
            }
        }
        true
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.peek()?;
        if !start.is_ascii_alphabetic() && start != '_' {
            return None;
        }
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                name.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        Some(name)
    }

    fn expr(&mut self) -> Result<Expr, GbnfError> {
        let mut alts = vec![self.concat()?];
        loop {
            self.skip();
            if self.peek() == Some('|') {
                self.pos += 1;
                self.skip();
                alts.push(self.concat()?);
            } else {
                break;
            }
        }
        if alts.len() == 1 {
            Ok(alts.pop().expect("one alternative"))
        } else {
            Ok(Expr::Alt(alts))
        }
    }

    fn concat(&mut self) -> Result<Expr, GbnfError> {
        let mut items = vec![self.repeat()?];
        loop {
            let before = self.pos;
            self.skip();
            match self.peek() {
                None | Some('|') | Some(')') => {
                    self.pos = before;
                    break;
                }
                Some('#') => {
                    self.pos = before;
                    self.skip();
                    continue;
                }
                _ => {
                    // A newline-separated rule name starting a new production
                    // is not part of this concatenation. Detect `ident ::=`.
                    if self.looks_like_rule_start() {
                        self.pos = before;
                        break;
                    }
                    items.push(self.repeat()?);
                }
            }
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one factor"))
        } else {
            Ok(Expr::Seq(items))
        }
    }

    fn looks_like_rule_start(&self) -> bool {
        let mut index = self.pos;
        if index >= self.chars.len() {
            return false;
        }
        let start = self.chars[index];
        if !start.is_ascii_alphabetic() && start != '_' {
            return false;
        }
        index += 1;
        while index < self.chars.len() {
            let ch = self.chars[index];
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                index += 1;
            } else {
                break;
            }
        }
        while index < self.chars.len() && self.chars[index].is_whitespace() {
            index += 1;
        }
        self.chars[index..].starts_with(&[':', ':', '='])
    }

    fn repeat(&mut self) -> Result<Expr, GbnfError> {
        let inner = self.primary()?;
        self.skip();
        Ok(match self.peek() {
            Some('*') => {
                self.pos += 1;
                Expr::Star(Box::new(inner))
            }
            Some('+') => {
                self.pos += 1;
                Expr::Plus(Box::new(inner))
            }
            Some('?') => {
                self.pos += 1;
                Expr::Opt(Box::new(inner))
            }
            _ => inner,
        })
    }

    fn primary(&mut self) -> Result<Expr, GbnfError> {
        self.skip();
        match self.peek() {
            Some('"') => self.literal(),
            Some('[') => self.class(),
            Some('.') => {
                self.pos += 1;
                Ok(Expr::Any)
            }
            Some('(') => {
                self.pos += 1;
                let expr = self.expr()?;
                self.skip();
                if self.bump() != Some(')') {
                    return Err(self.error("expected ')'"));
                }
                Ok(expr)
            }
            Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                let name = self.ident().expect("ident start was checked");
                Ok(Expr::Literal(format!("\0{name}")))
            }
            _ => Err(self.error("expected GBNF primary")),
        }
    }

    fn literal(&mut self) -> Result<Expr, GbnfError> {
        if self.bump() != Some('"') {
            return Err(self.error("expected string literal"));
        }
        let mut text = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error("unterminated GBNF literal")),
                Some('"') => return Ok(Expr::Literal(text)),
                Some('\\') => text.push(self.escape()?),
                Some(ch) => text.push(ch),
            }
        }
    }

    fn class(&mut self) -> Result<Expr, GbnfError> {
        if self.bump() != Some('[') {
            return Err(self.error("expected character class"));
        }
        let negated = if self.peek() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut items = Vec::new();
        loop {
            match self.bump() {
                None => return Err(self.error("unterminated GBNF character class")),
                Some(']') => {
                    return Ok(Expr::Class { negated, items });
                }
                Some('\\') => {
                    let ch = self.escape()?;
                    items.push(self.class_range(ch)?);
                }
                Some(ch) => items.push(self.class_range(ch)?),
            }
        }
    }

    fn class_range(&mut self, start: char) -> Result<ClassItem, GbnfError> {
        if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|ch| *ch != ']') {
            self.pos += 1;
            let end = match self.bump() {
                Some('\\') => self.escape()?,
                Some(ch) => ch,
                None => return Err(self.error("unterminated GBNF range")),
            };
            Ok(ClassItem::Range(start, end))
        } else {
            Ok(ClassItem::Char(start))
        }
    }

    fn escape(&mut self) -> Result<char, GbnfError> {
        match self.bump() {
            Some('n') => Ok('\n'),
            Some('r') => Ok('\r'),
            Some('t') => Ok('\t'),
            Some('\\') => Ok('\\'),
            Some('"') => Ok('"'),
            Some(']') => Ok(']'),
            Some('-') => Ok('-'),
            Some('x') => {
                let hi = self.bump().and_then(|ch| ch.to_digit(16));
                let lo = self.bump().and_then(|ch| ch.to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => char::from_u32((hi << 4) | lo)
                        .ok_or_else(|| self.error("invalid GBNF hex escape")),
                    _ => Err(self.error("invalid GBNF hex escape")),
                }
            }
            Some(ch) => Ok(ch),
            None => Err(self.error("unterminated GBNF escape")),
        }
    }
}

/// True when `input` is a complete match of `grammar`'s `root` rule.
pub fn matches(grammar: &Grammar, input: &str) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let mut memo: HashMap<(usize, usize), Option<usize>> = HashMap::new();
    match_rule(grammar, grammar.root, &chars, 0, &mut memo) == Some(chars.len())
}

fn match_rule(
    grammar: &Grammar,
    rule: usize,
    chars: &[char],
    pos: usize,
    memo: &mut HashMap<(usize, usize), Option<usize>>,
) -> Option<usize> {
    if let Some(hit) = memo.get(&(rule, pos)) {
        return *hit;
    }
    let result = match_expr(grammar, &grammar.rules[rule], chars, pos, memo);
    memo.insert((rule, pos), result);
    result
}

fn match_expr(
    grammar: &Grammar,
    expr: &Expr,
    chars: &[char],
    pos: usize,
    memo: &mut HashMap<(usize, usize), Option<usize>>,
) -> Option<usize> {
    match expr {
        Expr::Any => (pos < chars.len()).then_some(pos + 1),
        Expr::Literal(text) => {
            let mut cursor = pos;
            for expected in text.chars() {
                if chars.get(cursor) != Some(&expected) {
                    return None;
                }
                cursor += 1;
            }
            Some(cursor)
        }
        Expr::Class { negated, items } => {
            let ch = *chars.get(pos)?;
            let in_class = items.iter().any(|item| match item {
                ClassItem::Char(expected) => ch == *expected,
                ClassItem::Range(start, end) => ch >= *start && ch <= *end,
            });
            if in_class != *negated {
                Some(pos + 1)
            } else {
                None
            }
        }
        Expr::Seq(items) => {
            let mut cursor = pos;
            for item in items {
                cursor = match_expr(grammar, item, chars, cursor, memo)?;
            }
            Some(cursor)
        }
        Expr::Alt(items) => {
            for item in items {
                if let Some(end) = match_expr(grammar, item, chars, pos, memo) {
                    return Some(end);
                }
            }
            None
        }
        Expr::Star(inner) => match_repeat(grammar, inner, chars, pos, 0, None, memo),
        Expr::Plus(inner) => match_repeat(grammar, inner, chars, pos, 1, None, memo),
        Expr::Opt(inner) => match_repeat(grammar, inner, chars, pos, 0, Some(1), memo),
        Expr::Rule(index) => match_rule(grammar, *index, chars, pos, memo),
    }
}

fn match_repeat(
    grammar: &Grammar,
    inner: &Expr,
    chars: &[char],
    pos: usize,
    min: usize,
    max: Option<usize>,
    memo: &mut HashMap<(usize, usize), Option<usize>>,
) -> Option<usize> {
    let mut ends = vec![pos];
    let mut cursor = pos;
    while max.is_none_or(|limit| ends.len() - 1 < limit) {
        match match_expr(grammar, inner, chars, cursor, memo) {
            Some(next) if next > cursor => {
                ends.push(next);
                cursor = next;
            }
            _ => break,
        }
    }
    if ends.len() - 1 < min {
        return None;
    }
    Some(
        *ends
            .last()
            .expect("repeat always records the start position"),
    )
}

/// Parse `source` then match `input`.
#[cfg(test)]
pub fn parse_and_match(source: &str, input: &str) -> Result<bool, GbnfError> {
    Ok(matches(&parse(source)?, input))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(grammar: &str, input: &str) -> bool {
        parse_and_match(grammar, input).expect("test GBNF must parse")
    }

    #[test]
    fn packrat_matches_literals_classes_and_complete_input() {
        let grammar = r#"
            root ::= "ab" [0-9]+
        "#;
        assert!(
            accept(grammar, "ab42"),
            "concatenation must consume the whole input"
        );
        assert!(!accept(grammar, "ab"), "missing digits must fail");
        assert!(
            !accept(grammar, "ab42!"),
            "trailing junk must fail the complete-response match"
        );
    }

    #[test]
    fn ordered_choice_and_star_are_greedy_peg() {
        let grammar = r#"
            root ::= a*
            a ::= "aa" | "a"
        "#;
        assert!(
            accept(grammar, "aaa"),
            "star of ordered choice must cover an odd-length run"
        );
        assert!(accept(grammar, ""), "star may match empty");
    }

    #[test]
    fn comments_and_named_rules_parse() {
        let grammar = r#"
            # comment
            root ::= ident
            ident ::= [a-z]+
        "#;
        assert!(accept(grammar, "ok"));
        assert!(!accept(grammar, "OK"));
    }
}
