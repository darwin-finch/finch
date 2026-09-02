//! The Co-Forth lexer.
//!
//! All that survives of `interpreter.rs`. Nothing outside `#[cfg(test)]` ever
//! ran the VM it fed -- `--forth`, `--lisp`, `--exec` and `/forth` all dispatch
//! to the typed runtime -- so the interpreter, its builtins and its word table
//! were removed under #294. This function had the one live caller:
//! `programs::forth_definition_identity`, which hashes the token stream to give
//! a Forth definition a stable identity.
//!
//! Moved verbatim, including branches that recognise literal forms (`."`,
//! `s"`, `xlsx"`) whose consuming builtins are gone. They are not dead here:
//! this function's output *is* a program's identity, so changing how it splits
//! anything silently re-identifies every stored definition. Trimming them is a
//! deliberate decision with a migration, not tidying.

pub fn tokenize(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    let mut tok = String::new();

    macro_rules! flush {
        () => {
            if !tok.is_empty() {
                tokens.push(std::mem::take(&mut tok));
            }
        };
    }

    while let Some(&c) = chars.peek() {
        if c == '\\' && tok.is_empty() {
            // Standalone `\` — line comment: skip to end of line.
            // Only triggers when we are NOT mid-token (tok is empty).
            // This prevents `str:\` or other tokens containing backslash
            // from silently eating the rest of the line.
            chars.next();
            for c2 in chars.by_ref() {
                if c2 == '\n' {
                    break;
                }
            }
        } else if c == '(' && tok.is_empty() {
            // Standalone `(` — stack-comment or paren-comment.
            // Only triggers when NOT mid-token; `str::(` must not eat its trailing `)`.
            flush!();
            chars.next();
            // Skip until closing )
            let mut depth = 1;
            for c2 in chars.by_ref() {
                if c2 == '(' {
                    depth += 1;
                } else if c2 == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
        } else if c == '.' {
            chars.next();
            if chars.peek() == Some(&'"') {
                flush!();
                chars.next(); // consume "
                              // Skip exactly one space (standard Forth: space separates ." from content)
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                let mut s = String::new();
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                    s.push(c2);
                }
                tokens.push(format!("\x00str:{s}"));
            } else if chars.peek() == Some(&'|') {
                // .| text with "quotes" |  — print alternate delimiter
                flush!();
                chars.next(); // consume |
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                let mut s = String::new();
                for c2 in chars.by_ref() {
                    if c2 == '|' {
                        break;
                    }
                    s.push(c2);
                }
                tokens.push(format!("\x00str:{s}"));
            } else {
                // Sentence-final period: "square." → ["square", "."]
                // A trailing period (followed by space, newline, or end) is a separator —
                // every period executes, including natural language sentence endings.
                let next = chars.peek().copied();
                let is_sentence_end = matches!(
                    next,
                    None | Some(' ') | Some('\n') | Some('\r') | Some('\t') | Some(',')
                );
                if is_sentence_end && !tok.is_empty() {
                    flush!();
                    tokens.push(".".to_string()); // the . itself executes (print TOS or no-op)
                } else {
                    tok.push('.');
                }
            }
        } else if c == '"' && tok == "confirm" {
            // confirm" message" — like ." but emits Cell::Confirm instead of Cell::Str
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&' ') {
                chars.next();
            } // skip separator space
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00confirm:{s}"));
        } else if c == '"' && tok == "select" {
            // select" title|opt1|opt2" — pop-up dialog; pushes chosen index or -1
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&' ') {
                chars.next();
            } // skip separator space
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00select:{s}"));
        } else if c == '"' && tok == "read" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00read:{s}"));
        } else if c == '"' && tok == "csv" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00csv:{s}"));
        } else if c == '"' && tok == "tsv" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00tsv:{s}"));
        } else if c == '"' && tok == "xlsx" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00xlsx:{s}"));
        } else if c == '"' && tok == "exec" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00exec:{s}"));
        } else if c == '"' && tok == "glob" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00glob:{s}"));
        } else if c == '"' && tok == "peer" {
            // peer" addr"  — register a remote finch daemon as a scatter target
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00peer:{s}"));
        } else if c == '"' && tok == "ensemble-def" {
            // ensemble-def" name"  — snapshot current peers as a named ensemble
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("ensemble-def".to_string());
        } else if c == '"' && tok == "ensemble-use" {
            // ensemble-use" name"  — push peers, switch to named ensemble
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("ensemble-use".to_string());
        } else if c == '"' && tok == "registry" {
            // registry" addr"  — set registry address
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("registry-set".to_string());
        } else if c == '"' && tok == "join" {
            // join" addr"  — register this machine at addr with the configured registry
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("join-registry".to_string());
        } else if c == '"' && tok == "publish" {
            // publish" word-name"  — scatter word source to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("publish".to_string());
        } else if c == '"' && tok == "scatter" {
            // scatter" code"  — run code on all registered peers in parallel
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00scatter:{s}"));
        } else if c == '"' && tok == "symbol" {
            // symbol" name"  — share a word by name: if I know it, send my definition first;
            // then run the word on all peers so each speaks it in their own dialect
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00symbol:{s}"));
        } else if c == '"' && tok == "hello" {
            // hello" peer"  — send "hello from <hostname>!" to one peer by name or addr
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut peer = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                peer.push(c2);
            }
            tokens.push(format!("\x00hello:{peer}"));
        } else if c == '"' && tok == "tag" {
            // tag" name" "addr"  — label a peer's machine with a human name
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut name = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                name.push(c2);
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            }
            let mut addr = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                addr.push(c2);
            }
            tokens.push(format!("\x00tag:{name}\x01{addr}"));
        } else if c == '"' && tok == "channel" {
            // channel" #name"  — join a named channel; broadcast presence to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut name = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                name.push(c2);
            }
            tokens.push(format!("\x00channel:{name}"));
        } else if c == '"' && tok == "say" {
            // say" #channel" "message"  — send a message to a channel (all peers)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut chan = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                chan.push(c2);
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            }
            let mut msg = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                msg.push(c2);
            }
            tokens.push(format!("\x00say:{chan}\x01{msg}"));
        } else if c == '"' && tok == "zip" {
            // zip" src" "dest.zip"  — zip a file or directory into an archive
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut src = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                src.push(c2);
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            }
            let mut dest = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                dest.push(c2);
            }
            tokens.push(format!("\x00file-zip:{src}\x01{dest}"));
        } else if c == '"' && tok == "unzip" {
            // unzip" archive.zip" "dest/"  — extract a zip archive into a directory
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut src = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                src.push(c2);
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            }
            let mut dest = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                dest.push(c2);
            }
            tokens.push(format!("\x00file-unzip:{src}\x01{dest}"));
        } else if c == '"' && tok == "part" {
            // part" #name"  — leave a channel; broadcast departure to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut name = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                name.push(c2);
            }
            tokens.push(format!("\x00part:{name}"));
        } else if c == '"' && tok == "prove" {
            // prove" word"  — run test:<word> and show ✓ / ✗
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut word = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                word.push(c2);
            }
            tokens.push(format!("\x00prove:{word}"));
        } else if c == '"' && tok == "on" {
            // on" peer" "code"  — run code on exactly one peer (by address or label)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut peer = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                peer.push(c2);
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            }
            let mut code = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                code.push(c2);
            }
            tokens.push(format!("\x00on:{peer}\x01{code}"));
        } else if c == '"' && tok == "scatter-on" {
            // scatter-on" ensemble" "code"  — run code on named ensemble, no peer side-effects
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut ensemble = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                ensemble.push(c2);
            }
            // skip whitespace then opening quote for code
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
            } // consume opening "
            let mut code = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                code.push(c2);
            }
            tokens.push(format!("\x00scatter-on:{ensemble}\x01{code}"));
        } else if c == '"' && tok == "forth-back" {
            // forth-back" code"  — set Forth code to be executed on the caller after response
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00forth-back:{s}"));
        } else if c == '"' && tok == "s" {
            // s" text"  — push string pool index as integer operand (no printing)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
        } else if c == '"' && tok == "page" {
            // page"        — multiline proof page; content ends at " on its own line
            //   left side | right side
            //   ...
            // "
            tok.clear();
            chars.next(); // consume the opening "
            if chars.peek() == Some(&'\n') {
                chars.next();
            } // skip immediate newline
            let mut s = String::new();
            let mut at_line_start = true; // track whether current line so far is whitespace-only
            for c2 in chars.by_ref() {
                if c2 == '"' && at_line_start {
                    break;
                } // closing " on a line with only whitespace before it
                if c2 == '\n' {
                    at_line_start = true;
                } else if !c2.is_whitespace() && c2 != '"' {
                    at_line_start = false;
                }
                s.push(c2);
            }
            // trim trailing whitespace/newline from the block content
            let s = s.trim_end().to_string();
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("page".to_string());
        } else if c == '"' && tok == "resolve" {
            // resolve"   — many sentences, one truth; closing " alone on a line (or with leading whitespace)
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            let mut s = String::new();
            let mut at_line_start = true;
            for c2 in chars.by_ref() {
                if c2 == '"' && at_line_start {
                    break;
                }
                if c2 == '\n' {
                    at_line_start = true;
                } else if !c2.is_whitespace() {
                    at_line_start = false;
                }
                s.push(c2);
            }
            let s = s.trim_end().to_string();
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("resolve".to_string());
        } else if c == '|' && tok == "s" {
            // s| text with "quotes" |  — alternate string delimiter; avoids escaping hell
            tok.clear();
            chars.next(); // consume |
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '|' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
        } else if c == '"' && tok == "boot" {
            // boot" text"  — register a line to print at every boot; persisted to ~/.finch/boot.forth
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("register-boot".to_string());
        } else if c == '"' && tok == "gen" {
            // gen" prompt"  — call AI generator, emit response
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00gen:{s}"));
        } else if c == '"' && tok == "scatter-exec" {
            // scatter-exec" cmd"  — run bash -c cmd on all peers via /v1/exec
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            tokens.push(format!("\x00scatter-exec:{s}"));
        } else if c == ';' {
            // `;` always terminates the current token and emits itself as a standalone token.
            // This lets it be a sentence separator in natural language: "Hello; I am forth."
            flush!();
            tokens.push(";".to_string());
            chars.next();
        } else if c.is_whitespace() {
            flush!();
            chars.next();
        } else if c == '\'' {
            // Apostrophe in natural-language contractions: that's → thats, we're → were.
            // Skip it — the token accumulates without the apostrophe.
            chars.next();
        } else {
            tok.push(c);
            chars.next();
        }
    }
    flush!();
    tokens
}

#[cfg(test)]
mod tests {
    use super::tokenize;

    /// A golden token stream, because this output is a program's identity.
    ///
    /// `programs::from_source_file` derives a definition's name from the token
    /// after `:`, so how this function splits anything is load-bearing for every
    /// stored definition. The branches for `."`, `s"` and the rest survived
    /// #294 on that argument alone, with a doc comment as their only guard --
    /// which means the next "tidy the dead literal forms" commit passes CI.
    ///
    /// This pins them. If a change here is deliberate, this test is where the
    /// migration argument gets made.
    #[test]
    fn test_tokenize_splits_the_literal_forms_it_always_has() {
        assert_eq!(
            tokenize(": double dup + ;"),
            vec![":", "double", "dup", "+", ";"]
        );

        // `."` and `s"` each collapse to one sentinel-tagged token carrying the
        // literal, with the single separating space consumed. The `\0` prefix is
        // what kept the literal from colliding with a word of the same text.
        assert_eq!(
            tokenize(r#"." hello there" cr"#),
            vec!["\0str:hello there", "cr"]
        );
        assert_eq!(tokenize(r#"s" a b c""#), vec!["\0push-str:a b c"]);

        // A standalone backslash opens a line comment; one inside a token does
        // not, so `str:\` survives as itself.
        assert_eq!(tokenize("1 \\ ignored\n2"), vec!["1", "2"]);
        assert_eq!(tokenize("str:\\ 1"), vec!["str:\\", "1"]);

        // A standalone `(` opens a paren comment and nests.
        assert_eq!(tokenize("1 ( a ( b ) c ) 2"), vec!["1", "2"]);

        // A trailing period splits off as its own token. (The REPL's rule that
        // a sentence-ending period routes to NL rather than Forth lives a layer
        // above this, in `repl_event::event_loop`.)
        assert_eq!(tokenize("dont. go"), vec!["dont", ".", "go"]);
    }

    /// The rest of the sentinel-tagged literal forms.
    ///
    /// The doc above names `."`, `s"` and `xlsx"` as the forms preserved on the
    /// identity argument, and the first version of this test covered two of
    /// them. All follow one shape -- `\0<tag>:<literal>`, one token, the single
    /// separating space consumed -- so pinning the shape across the family is
    /// what stops a "tidy the dead literal forms" commit passing CI.
    #[test]
    fn test_tokenize_tags_each_literal_form_with_its_own_sentinel() {
        for (source, expected) in [
            (r#"xlsx" a.xlsx""#, "\u{0}xlsx:a.xlsx"),
            (r#"csv" a.csv""#, "\u{0}csv:a.csv"),
            (r#"tsv" a.tsv""#, "\u{0}tsv:a.tsv"),
            (r#"read" f""#, "\u{0}read:f"),
            (r#"confirm" ok?""#, "\u{0}confirm:ok?"),
            (r#"select" a|b""#, "\u{0}select:a|b"),
            (r#"exec" ls""#, "\u{0}exec:ls"),
            (r#"glob" *.rs""#, "\u{0}glob:*.rs"),
        ] {
            assert_eq!(tokenize(source), vec![expected], "for {source:?}");
        }
    }

    /// Two rules that silently rewrite a token, and so its identity.
    #[test]
    fn test_tokenize_elides_apostrophes_but_keeps_an_interior_period() {
        // A contraction loses its apostrophe, so `that's` and `thats` are the
        // same token -- and the same definition.
        assert_eq!(tokenize("that's"), vec!["thats"]);
        // But a period *inside* a token stays, so a decimal survives whole
        // while a trailing period splits.
        assert_eq!(tokenize("3.14"), vec!["3.14"]);
        assert_eq!(tokenize("3.14."), vec!["3.14", "."]);
    }

    /// Whitespace shape must not change identity.
    #[test]
    fn test_tokenize_is_insensitive_to_surrounding_whitespace() {
        assert_eq!(tokenize("  : a b ;  "), tokenize(": a b ;"));
        assert_eq!(tokenize(": a\n  b\t;"), tokenize(": a b ;"));
    }
}
