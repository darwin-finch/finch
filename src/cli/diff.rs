//! Theme-aware, structured rendering of unified file diffs.

use crate::config::{ColorScheme, MessageBand};
use ratatui::style::Color;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub binary: bool,
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
    /// Build a valid single-hunk diff for a complete-file replacement.
    pub fn from_texts(path: &str, old: &str, new: &str) -> Self {
        let old_lines: Vec<_> = old.lines().collect();
        let new_lines: Vec<_> = new.lines().collect();
        let mut prefix = 0;
        while prefix < old_lines.len().min(new_lines.len())
            && old_lines[prefix] == new_lines[prefix]
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix
            < old_lines
                .len()
                .saturating_sub(prefix)
                .min(new_lines.len().saturating_sub(prefix))
            && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let start = prefix.saturating_sub(3);
        let old_end = (old_lines.len() - suffix + 3).min(old_lines.len());
        let new_end = (new_lines.len() - suffix + 3).min(new_lines.len());
        let mut lines = Vec::new();
        lines.extend(old_lines[start..prefix].iter().map(|s| DiffLine {
            kind: DiffLineKind::Context,
            text: (*s).to_string(),
        }));
        lines.extend(
            old_lines[prefix..old_lines.len() - suffix]
                .iter()
                .map(|s| DiffLine {
                    kind: DiffLineKind::Remove,
                    text: (*s).to_string(),
                }),
        );
        lines.extend(
            new_lines[prefix..new_lines.len() - suffix]
                .iter()
                .map(|s| DiffLine {
                    kind: DiffLineKind::Add,
                    text: (*s).to_string(),
                }),
        );
        lines.extend(
            old_lines[old_lines.len() - suffix..old_end]
                .iter()
                .map(|s| DiffLine {
                    kind: DiffLineKind::Context,
                    text: (*s).to_string(),
                }),
        );
        Self {
            old_path: path.to_string(),
            new_path: path.to_string(),
            binary: false,
            hunks: vec![DiffHunk {
                old_start: start + 1,
                old_count: old_end - start,
                new_start: start + 1,
                new_count: new_end - start,
                context: String::new(),
                lines,
            }],
        }
    }

    /// Encode the structured value as conventional unified diff text.
    pub fn to_unified(&self) -> String {
        let mut out = format!("--- a/{}\n+++ b/{}\n", self.old_path, self.new_path);
        for h in &self.hunks {
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@{}\n",
                h.old_start, h.old_count, h.new_start, h.new_count, h.context
            ));
            for l in &h.lines {
                let marker = match l.kind {
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Add => '+',
                    DiffLineKind::Remove => '-',
                    DiffLineKind::NoNewline => '\\',
                };
                out.push(marker);
                out.push_str(&l.text);
                out.push('\n');
            }
        }
        out
    }
    pub fn parse(text: &str) -> Option<Self> {
        let mut old_path = String::new();
        let mut new_path = String::new();
        let mut binary = false;
        let mut hunks = Vec::new();
        let mut current: Option<DiffHunk> = None;
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("--- ") {
                old_path = clean_path(path);
            } else if let Some(path) = line.strip_prefix("+++ ") {
                new_path = clean_path(path);
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                binary = true;
            } else if line.starts_with("@@") {
                if let Some(h) = current.take() {
                    hunks.push(h);
                }
                current = parse_hunk_header(line);
            } else if let Some(h) = current.as_mut() {
                let (kind, text) = if let Some(s) = line.strip_prefix('+') {
                    (DiffLineKind::Add, s)
                } else if let Some(s) = line.strip_prefix('-') {
                    (DiffLineKind::Remove, s)
                } else if let Some(s) = line.strip_prefix(' ') {
                    (DiffLineKind::Context, s)
                } else if line == "\\ No newline at end of file" {
                    (DiffLineKind::NoNewline, line)
                } else {
                    continue;
                };
                h.lines.push(DiffLine {
                    kind,
                    text: text.to_string(),
                });
            }
        }
        if let Some(h) = current {
            hunks.push(h);
        }
        if old_path.is_empty() && new_path.is_empty() {
            return None;
        }
        Some(Self {
            old_path,
            new_path,
            binary,
            hunks,
        })
    }

    pub fn added(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Add)
            .count()
    }
    pub fn removed(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Remove)
            .count()
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
        let mut out = String::new();
        let meta = if self.is_rename() {
            format!("{} → {}", self.old_path, self.new_path)
        } else {
            self.display_path().to_string()
        };
        out.push_str(&format!("{}  +{} -{}", meta, self.added(), self.removed()));
        if self.binary {
            out.push_str("  binary");
        }
        if self.is_rename() {
            out.push_str("  renamed");
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
                    h.old_start, h.old_count, h.new_start, h.new_count, h.context
                ),
                colors,
                mode,
                Tone::Hunk,
            ));
            let (mut old, mut new) = (h.old_start, h.new_start);
            for line in &h.lines {
                let (old_num, new_num, marker, tone) = match line.kind {
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
                let left = old_num.map(|n| n.to_string()).unwrap_or_default();
                let right = new_num.map(|n| n.to_string()).unwrap_or_default();
                out.push_str(&paint(
                    format!(
                        "{:>width$} {:>width$} {} {}",
                        left,
                        right,
                        marker,
                        line.text,
                        width = width
                    ),
                    colors,
                    mode,
                    tone,
                ));
            }
        }
        out
    }
}

fn clean_path(path: &str) -> String {
    path.split('\t')
        .next()
        .unwrap_or(path)
        .trim()
        .trim_start_matches("a/")
        .trim_start_matches("b/")
        .to_string()
}
fn range(part: &str) -> Option<(usize, usize)> {
    let mut p = part[1..].split(',');
    Some((
        p.next()?.parse().ok()?,
        p.next().and_then(|v| v.parse().ok()).unwrap_or(1),
    ))
}
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let end = line[2..].find("@@")? + 2;
    let mut p = line[2..end].split_whitespace();
    let (old_start, old_count) = range(p.next()?)?;
    let (new_start, new_count) = range(p.next()?)?;
    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        context: line[end + 2..].to_string(),
        lines: Vec::new(),
    })
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
    let dark = matches!(colors.message_band_style(MessageBand::Tool).fg,Some(Color::Rgb(r,g,b)) if (r as u32*299+g as u32*587+b as u32*114)/1000>127);
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
    fn test_parse_header_counts_hunks_and_rename() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        assert_eq!((d.added(), d.removed()), (2, 1));
        assert!(d.is_rename());
        assert_eq!(d.hunks[0].context, " fn x");
    }
    #[test]
    fn test_no_color_is_accessible_and_numbered() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        let s = d.render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert_eq!(
            s,
            concat!(
                "src/old.rs → src/new.rs  +2 -1  renamed\n",
                "@@ -2,2 +2,3 @@ fn x\n",
                "2 2   keep\n",
                "3   - old\n",
                "  3 + new\n",
                "  4 + more"
            )
        );
    }
    #[test]
    fn test_light_and_dark_choose_distinct_composition() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        let dark = d.render(&ColorTheme::Dark.to_scheme(), DiffColorMode::Theme);
        let light = d.render(&ColorTheme::Light.to_scheme(), DiffColorMode::Theme);
        assert_ne!(dark, light);
        assert!(dark.contains("38;2;126;231;135"));
        assert!(light.contains("38;2;0;92;38"));
    }
    #[test]
    fn test_binary_metadata() {
        let d =
            FileDiff::parse("--- a/a.png\n+++ b/b.png\nBinary files a/a.png and b/b.png differ\n")
                .unwrap();
        let s = d.render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(s.contains("binary"));
        assert!(s.contains("renamed"));
    }
}
