//! Bounded, terminal-safe structured file diffs.

use crate::config::{ColorScheme, MessageBand};
use ratatui::style::Color;
use similar::TextDiff;

pub const MAX_DIFF_INPUT_BYTES: usize = 1_048_576;
pub const MAX_DIFF_LINES: usize = 400;
pub const MAX_DIFF_FILES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub binary: bool,
    pub elided: Option<String>,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub context: String,
    pub lines: Vec<DiffLine>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Remove,
    NoNewline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffColorMode {
    Theme,
    NoColor,
}

impl FileDiff {
    pub fn from_texts(path: &str, old: &str, new: &str) -> Self {
        let path = sanitize_terminal(path);
        if old.contains('\0') || new.contains('\0') {
            return Self {
                old_path: path.clone(),
                new_path: path,
                binary: true,
                elided: Some("binary content omitted".into()),
                hunks: vec![],
            };
        }
        if old.len().saturating_add(new.len()) > MAX_DIFF_INPUT_BYTES {
            return Self {
                old_path: path.clone(),
                new_path: path,
                binary: false,
                elided: Some(format!(
                    "change omitted ({} bytes exceeds display limit)",
                    old.len().saturating_add(new.len())
                )),
                hunks: vec![],
            };
        }
        let unified = TextDiff::from_lines(old, new)
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();
        let mut parsed = Self::parse(&unified).unwrap_or(Self {
            old_path: path.clone(),
            new_path: path,
            binary: false,
            elided: None,
            hunks: vec![],
        });
        let old_ending = line_ending(old);
        let new_ending = line_ending(new);
        if old_ending != new_ending && old_ending.is_some() && new_ending.is_some() {
            parsed.elided = Some(format!(
                "line endings {} → {}",
                old_ending.unwrap(),
                new_ending.unwrap()
            ));
        }
        parsed
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_all(text).into_iter().next()
    }
    pub fn parse_all(text: &str) -> Vec<Self> {
        let mut files = vec![];
        let mut file: Option<Self> = None;
        let mut hunk: Option<DiffHunk> = None;
        let mut accepted_lines = 0usize;
        fn flush_hunk(file: &mut Option<FileDiff>, hunk: &mut Option<DiffHunk>) {
            if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
                f.hunks.push(h)
            }
        }
        fn flush_file(
            files: &mut Vec<FileDiff>,
            file: &mut Option<FileDiff>,
            hunk: &mut Option<DiffHunk>,
        ) {
            flush_hunk(file, hunk);
            if let Some(f) = file.take() {
                if !f.old_path.is_empty() || !f.new_path.is_empty() {
                    files.push(f)
                }
            }
        }
        for raw in text.lines() {
            if files.len() >= MAX_DIFF_FILES {
                break;
            }
            let line = raw.trim_end_matches('\r');
            if line.starts_with("diff --git ") {
                flush_file(&mut files, &mut file, &mut hunk);
                file = Some(empty_file());
                continue;
            }
            if let Some(path) = line.strip_prefix("--- ") {
                if file.as_ref().is_some_and(|f| {
                    !f.old_path.is_empty() && (!f.hunks.is_empty() || hunk.is_some())
                }) {
                    flush_file(&mut files, &mut file, &mut hunk)
                }
                file.get_or_insert_with(empty_file).old_path = parse_path(path);
                continue;
            }
            if let Some(path) = line.strip_prefix("+++ ") {
                file.get_or_insert_with(empty_file).new_path = parse_path(path);
                continue;
            }
            if let Some(path) = line.strip_prefix("rename from ") {
                file.get_or_insert_with(empty_file).old_path = parse_path(path);
                continue;
            }
            if let Some(path) = line.strip_prefix("rename to ") {
                file.get_or_insert_with(empty_file).new_path = parse_path(path);
                continue;
            }
            if line.starts_with("Binary files ") || line == "GIT binary patch" {
                let f = file.get_or_insert_with(empty_file);
                f.binary = true;
                f.elided = Some("binary content omitted".into());
                continue;
            }
            if line.starts_with("@@") {
                flush_hunk(&mut file, &mut hunk);
                hunk = parse_hunk_header(line);
                continue;
            }
            if let Some(note) = line.strip_prefix("# finch: ") {
                file.get_or_insert_with(empty_file).elided = Some(sanitize_terminal(note));
                continue;
            }
            if let Some(h) = hunk.as_mut() {
                let (kind, value) = if line == "\\ No newline at end of file" {
                    (DiffLineKind::NoNewline, "No newline at end of file")
                } else if let Some(v) = line.strip_prefix('+') {
                    (DiffLineKind::Add, v)
                } else if let Some(v) = line.strip_prefix('-') {
                    (DiffLineKind::Remove, v)
                } else if let Some(v) = line.strip_prefix(' ') {
                    (DiffLineKind::Context, v)
                } else {
                    continue;
                };
                if accepted_lines < MAX_DIFF_LINES {
                    h.lines.push(DiffLine {
                        kind,
                        text: sanitize_terminal(value),
                    });
                    accepted_lines += 1;
                } else {
                    file.get_or_insert_with(empty_file).elided =
                        Some(format!("diff truncated at {MAX_DIFF_LINES} lines"))
                }
            }
        }
        flush_file(&mut files, &mut file, &mut hunk);
        files
    }

    pub fn to_unified(&self) -> String {
        let mut out = format!(
            "--- {}\n+++ {}\n",
            encode_path(&self.old_path, 'a'),
            encode_path(&self.new_path, 'b')
        );
        if self.binary {
            out.push_str(&format!(
                "Binary files {} and {} differ\n",
                self.old_path, self.new_path
            ));
            return out;
        }
        for h in &self.hunks {
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@{}\n",
                h.old_start, h.old_count, h.new_start, h.new_count, h.context
            ));
            for l in &h.lines {
                if l.kind == DiffLineKind::NoNewline {
                    out.push_str("\\ No newline at end of file\n")
                } else {
                    out.push(match l.kind {
                        DiffLineKind::Context => ' ',
                        DiffLineKind::Add => '+',
                        DiffLineKind::Remove => '-',
                        DiffLineKind::NoNewline => unreachable!(),
                    });
                    out.push_str(&l.text);
                    out.push('\n')
                }
            }
        }
        if let Some(v) = &self.elided {
            out.push_str(&format!("# finch: {}\n", sanitize_terminal(v)))
        }
        out
    }
    pub fn added(&self) -> usize {
        count(self, DiffLineKind::Add)
    }
    pub fn removed(&self) -> usize {
        count(self, DiffLineKind::Remove)
    }
    pub fn display_path(&self) -> &str {
        if self.new_path != "/dev/null" && !self.new_path.is_empty() {
            &self.new_path
        } else {
            &self.old_path
        }
    }
    pub fn is_rename(&self) -> bool {
        self.old_path != self.new_path
            && self.old_path != "/dev/null"
            && self.new_path != "/dev/null"
    }
    pub fn render(&self, colors: &ColorScheme, mode: DiffColorMode) -> String {
        let meta = if self.is_rename() {
            format!("{} → {}", self.old_path, self.new_path)
        } else {
            self.display_path().into()
        };
        let mut out = format!(
            "{}  +{} -{}",
            sanitize_terminal(&meta),
            self.added(),
            self.removed()
        );
        if self.binary {
            out.push_str("  binary")
        }
        if self.is_rename() {
            out.push_str("  renamed")
        }
        if let Some(v) = &self.elided {
            out.push_str(&format!("  [{}]", sanitize_terminal(v)))
        }
        let width = self
            .hunks
            .iter()
            .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        for h in &self.hunks {
            out.push('\n');
            out.push_str(&paint(
                format!(
                    "@@ -{},{} +{},{} @@{}",
                    h.old_start,
                    h.old_count,
                    h.new_start,
                    h.new_count,
                    sanitize_terminal(&h.context)
                ),
                colors,
                mode,
                Tone::Hunk,
            ));
            let (mut old, mut new) = (h.old_start, h.new_start);
            for line in &h.lines {
                let (a, b, m, t) = match line.kind {
                    DiffLineKind::Context => {
                        let v = (Some(old), Some(new), ' ', Tone::Context);
                        old += 1;
                        new += 1;
                        v
                    }
                    DiffLineKind::Remove => {
                        let v = (Some(old), None, '-', Tone::Remove);
                        old += 1;
                        v
                    }
                    DiffLineKind::Add => {
                        let v = (None, Some(new), '+', Tone::Add);
                        new += 1;
                        v
                    }
                    DiffLineKind::NoNewline => (None, None, '\\', Tone::Meta),
                };
                out.push('\n');
                let a = a.map(|v| v.to_string()).unwrap_or_default();
                let b = b.map(|v| v.to_string()).unwrap_or_default();
                out.push_str(&paint(
                    format!(
                        "{:>width$} {:>width$} {} {}",
                        a,
                        b,
                        m,
                        sanitize_terminal(&line.text),
                        width = width
                    ),
                    colors,
                    mode,
                    t,
                ))
            }
        }
        out
    }
}

fn empty_file() -> FileDiff {
    FileDiff {
        old_path: String::new(),
        new_path: String::new(),
        binary: false,
        elided: None,
        hunks: vec![],
    }
}
fn line_ending(value: &str) -> Option<&'static str> {
    if value.contains("\r\n") {
        Some("CRLF")
    } else if value.contains('\n') {
        Some("LF")
    } else {
        None
    }
}
fn count(d: &FileDiff, k: DiffLineKind) -> usize {
    d.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == k)
        .count()
}
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let end = line.get(2..)?.find("@@")? + 2;
    let mut p = line.get(2..end)?.split_whitespace();
    let (old_start, old_count) = range(p.next()?)?;
    let (new_start, new_count) = range(p.next()?)?;
    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        context: sanitize_terminal(line.get(end + 2..).unwrap_or("")),
        lines: vec![],
    })
}
fn range(s: &str) -> Option<(usize, usize)> {
    let mut p = s.get(1..)?.split(',');
    Some((
        p.next()?.parse().ok()?,
        p.next().and_then(|v| v.parse().ok()).unwrap_or(1),
    ))
}
fn parse_path(raw: &str) -> String {
    let raw = raw.split('\t').next().unwrap_or(raw).trim();
    let decoded = if raw.starts_with('"') && raw.ends_with('"') {
        unquote(&raw[1..raw.len() - 1])
    } else {
        raw.into()
    };
    sanitize_terminal(
        decoded
            .strip_prefix("a/")
            .or_else(|| decoded.strip_prefix("b/"))
            .unwrap_or(&decoded),
    )
}
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        if index >= bytes.len() {
            out.push(b'\\');
            break;
        }
        match bytes[index] {
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            digit @ b'0'..=b'7' => {
                let mut value = digit - b'0';
                for _ in 0..2 {
                    if index + 1 >= bytes.len() || !(b'0'..=b'7').contains(&bytes[index + 1]) {
                        break;
                    }
                    index += 1;
                    value = value.saturating_mul(8).saturating_add(bytes[index] - b'0');
                }
                out.push(value);
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn encode_path(path: &str, prefix: char) -> String {
    let p = if path == "/dev/null" {
        path.into()
    } else {
        format!("{prefix}/{path}")
    };
    if p.chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        format!(
            "\"{}\"",
            p.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\t', "\\t")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    } else {
        p
    }
}

pub fn sanitize_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek().copied() {
                Some('[') => {
                    it.next();
                    for x in it.by_ref() {
                        if ('@'..='~').contains(&x) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    let mut esc = false;
                    for x in it.by_ref() {
                        if x == '\x07' || (esc && x == '\\') {
                            break;
                        }
                        esc = x == '\x1b'
                    }
                }
                _ => {}
            }
            continue;
        }
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('�'),
            c => out.push(c),
        }
    }
    out
}
enum Tone {
    Add,
    Remove,
    Hunk,
    Context,
    Meta,
}
fn paint(text: String, colors: &ColorScheme, mode: DiffColorMode, tone: Tone) -> String {
    if mode == DiffColorMode::NoColor {
        return text;
    }
    let dark = matches!(colors.message_band_style(MessageBand::Tool).fg,Some(Color::Rgb(r,g,b))if(r as u32*299+g as u32*587+b as u32*114)/1000>127);
    let (r, g, b) = match (dark, tone) {
        (true, Tone::Add) => (126, 231, 135),
        (true, Tone::Remove) => (255, 123, 114),
        (true, Tone::Hunk) => (121, 192, 255),
        (true, Tone::Meta) => (139, 148, 158),
        (true, Tone::Context) => (245, 247, 250),
        (false, Tone::Add) => (0, 92, 38),
        (false, Tone::Remove) => (179, 29, 40),
        (false, Tone::Hunk) => (5, 80, 174),
        (false, Tone::Meta) => (87, 96, 106),
        (false, Tone::Context) => (18, 22, 28),
    };
    format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    const SAMPLE: &str =
        "--- a/src/old.rs\n+++ b/src/new.rs\n@@ -2,2 +2,3 @@ fn x\n keep\n-old\n+new\n+more\n";
    #[test]
    fn no_color_snapshot() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        assert_eq!(d.render(&ColorScheme::default(),DiffColorMode::NoColor),"src/old.rs → src/new.rs  +2 -1  renamed\n@@ -2,2 +2,3 @@ fn x\n2 2   keep\n3   - old\n  3 + new\n  4 + more")
    }
    #[test]
    fn light_dark() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        assert_ne!(
            d.render(&ColorTheme::Dark.to_scheme(), DiffColorMode::Theme),
            d.render(&ColorTheme::Light.to_scheme(), DiffColorMode::Theme)
        )
    }
    #[test]
    fn multi_file_paths_do_not_bleed() {
        let d = FileDiff::parse_all(
            "--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-m\n+n\n",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].display_path(), "a");
        assert_eq!(d[1].display_path(), "b")
    }
    #[test]
    fn quoted_rename_roundtrip() {
        let d=FileDiff::parse("diff --git \"a/old name\" \"b/new name\"\nrename from \"old name\"\nrename to \"new name\"\n").unwrap();
        assert_eq!(d.old_path, "old name");
        assert_eq!(d.new_path, "new name");
        assert!(FileDiff::parse(&d.to_unified()).unwrap().is_rename())
    }

    #[test]
    fn git_octal_quoted_utf8_path_is_decoded() {
        let diff =
            FileDiff::parse("--- \"a/caf\\303\\251.txt\"\n+++ \"b/caf\\303\\251.txt\"\n").unwrap();
        assert_eq!(diff.display_path(), "café.txt");
    }
    #[test]
    fn distant_changes_make_multiple_hunks() {
        let old = (0..30).map(|i| format!("{i}\n")).collect::<String>();
        let new = old
            .replacen("2\n", "two\n", 1)
            .replacen("27\n", "twenty-seven\n", 1);
        assert!(FileDiff::from_texts("x", &old, &new).hunks.len() >= 2)
    }
    #[test]
    fn newline_and_crlf_are_visible() {
        let lf = FileDiff::from_texts("x", "a\n", "a");
        assert!(lf
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.kind == DiffLineKind::NoNewline));
        let crlf = FileDiff::from_texts("x", "a\r\n", "a\n");
        assert!(crlf.removed() > 0 && crlf.added() > 0)
    }
    #[test]
    fn hostile_and_unicode_are_safe() {
        let d = FileDiff::from_texts("é\x1b]8;;bad\x07x", "a\n", "\x1b[31mé\0");
        let s = d.render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(!s.contains('\x1b'));
        assert!(s.contains('é'));
        assert!(d.binary)
    }
    #[test]
    fn large_diff_is_elided() {
        let x = "a".repeat(MAX_DIFF_INPUT_BYTES);
        assert!(FileDiff::from_texts("x", &x, &x).elided.is_some())
    }
}
