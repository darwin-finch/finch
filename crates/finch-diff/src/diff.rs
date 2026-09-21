//! Bounded, terminal-safe structured file diffs.

use finch_theme::{ColorScheme, MessageBand};
use ratatui::style::Color;
use similar::{ChangeTag, TextDiff};
use std::time::Duration;

pub const MAX_DIFF_INPUT_BYTES: usize = 1_048_576;
/// Maximum semantic lines retained for a bounded diff. This is separate
/// from the much smaller transcript preview limit.
pub const MAX_DIFF_LINES: usize = 1024;
pub const MAX_DIFF_PREVIEW_LINES: usize = 16;
pub const MAX_DIFF_FILES: usize = 64;
pub const MAX_DIFF_LINE_CHARS: usize = 512;
pub const MAX_DIFF_COMPUTE_LINES: usize = 20_000;
pub const MAX_DIFF_HUNKS: usize = 128;
pub const MAX_DIFF_STRUCTURAL_LINES: usize = 1024;
pub const MAX_RENDER_CHARS: usize = 131_072;

/// Bounded structured diff for one file.
///
/// Completeness is asked through [`Self::is_complete`]: it is the conjunction
/// of [`Self::counts_are_exact`], [`Self::file_count_is_exact`], retained-model
/// elision, and whether canonical [`Self::to_unified`] rendering would hit
/// [`MAX_RENDER_CHARS`]. `elided` may still hold an informational line-ending
/// note on an otherwise complete diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub binary: bool,
    /// Why retained hunk text or files were omitted, or an informational
    /// line-ending note. Render-time truncation of [`Self::to_unified`] is
    /// reported by [`Self::counts_are_exact`] / [`Self::is_complete`], not
    /// only by this field.
    pub elided: Option<String>,
    pub hunks: Vec<DiffHunk>,
    total_added: usize,
    total_removed: usize,
    totals_exact: bool,
    file_count_exact: bool,
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
impl DiffColorMode {
    /// Select the terminal mode used by interactive production renderers.
    pub fn production() -> Self {
        Self::for_environment(
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    fn for_environment(no_color: bool, term: Option<&str>) -> Self {
        if no_color || term == Some("dumb") {
            Self::NoColor
        } else {
            Self::Theme
        }
    }
}

impl FileDiff {
    pub fn from_texts(path: &str, old: &str, new: &str) -> Self {
        Self::from_texts_with_paths(path, path, old, new)
    }

    /// Build a diff for a newly created file, preserving `/dev/null` as the
    /// old path through canonical serialization and retained rendering.
    pub fn from_created(path: &str, new: &str) -> Self {
        Self::from_texts_with_paths("/dev/null", path, "", new)
    }

    fn from_texts_with_paths(old_path: &str, new_path: &str, old: &str, new: &str) -> Self {
        let old_path = sanitize_terminal(old_path);
        let new_path = sanitize_terminal(new_path);
        if old.contains('\0') || new.contains('\0') {
            return Self {
                old_path,
                new_path,
                binary: true,
                elided: Some("binary content omitted".into()),
                hunks: vec![],
                total_added: 0,
                total_removed: 0,
                totals_exact: true,
                file_count_exact: true,
            };
        }
        if old.len().saturating_add(new.len()) > MAX_DIFF_INPUT_BYTES {
            return Self {
                old_path,
                new_path,
                binary: false,
                elided: Some(format!(
                    "change omitted ({} bytes exceeds display limit)",
                    old.len().saturating_add(new.len())
                )),
                hunks: vec![],
                total_added: 0,
                total_removed: 0,
                totals_exact: false,
                file_count_exact: true,
            };
        }
        let input_lines = old.lines().count().saturating_add(new.lines().count());
        if input_lines > MAX_DIFF_COMPUTE_LINES {
            return Self {
                old_path,
                new_path,
                binary: false,
                elided: Some(format!(
                    "change omitted ({input_lines} lines exceeds diff computation limit)"
                )),
                hunks: vec![],
                total_added: 0,
                total_removed: 0,
                totals_exact: false,
                file_count_exact: true,
            };
        }
        let text_diff = TextDiff::configure()
            .timeout(Duration::from_millis(50))
            .diff_lines(old, new);
        let mut file = Self {
            old_path,
            new_path,
            binary: false,
            elided: None,
            hunks: vec![],
            total_added: 0,
            total_removed: 0,
            totals_exact: true,
            file_count_exact: true,
        };
        file.ingest_similar(&text_diff);
        let old_ending = line_ending(old);
        let new_ending = line_ending(new);
        if old_ending != new_ending && old_ending.is_some() && new_ending.is_some() {
            file.record_elision(format!(
                "line endings {} → {}",
                old_ending.unwrap(),
                new_ending.unwrap()
            ));
        }
        file.mark_truncated_rendering();
        file
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_all(text).into_iter().next()
    }
    pub fn parse_all(text: &str) -> Vec<Self> {
        if text.len() > MAX_DIFF_INPUT_BYTES {
            return vec![elided_input("diff input exceeded display byte limit")];
        }
        let mut files = vec![];
        let mut file: Option<Self> = None;
        let mut hunk: Option<DiffHunk> = None;
        let mut accepted_lines = 0usize;
        let mut structural_lines = 0usize;
        let mut accepted_hunks = 0usize;
        let mut git_header_awaits_file_headers = false;
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
                if files.len() < MAX_DIFF_FILES
                    && (!f.old_path.is_empty() || !f.new_path.is_empty())
                {
                    files.push(f)
                }
            }
        }
        for raw in text.lines() {
            structural_lines += 1;
            if structural_lines > MAX_DIFF_STRUCTURAL_LINES {
                let current = file.get_or_insert_with(empty_file);
                current.record_elision(
                    "diff exceeded structural line limit; later files may be omitted",
                );
                current.totals_exact = false;
                current.file_count_exact = false;
                break;
            }
            let line = raw.trim_end_matches('\r');
            let in_open_hunk = hunk.as_ref().is_some_and(hunk_body_is_open);
            if let Some(paths) = line.strip_prefix("diff --git ") {
                if files.len().saturating_add(usize::from(file.is_some())) >= MAX_DIFF_FILES {
                    let current = file.get_or_insert_with(empty_file);
                    current.record_elision("additional files omitted at file limit");
                    current.totals_exact = false;
                    current.file_count_exact = false;
                    break;
                }
                flush_file(&mut files, &mut file, &mut hunk);
                let mut next = empty_file();
                if let Some((old, new)) = parse_git_paths(paths) {
                    next.old_path = old;
                    next.new_path = new;
                }
                file = Some(next);
                git_header_awaits_file_headers = true;
                continue;
            }
            if !in_open_hunk {
                if let Some(path) = line.strip_prefix("--- ") {
                    if !git_header_awaits_file_headers
                        && file
                            .as_ref()
                            .is_some_and(|f| !f.old_path.is_empty() && !f.new_path.is_empty())
                    {
                        if files.len().saturating_add(1) >= MAX_DIFF_FILES {
                            let current = file.as_mut().expect("checked existing file");
                            current.record_elision("additional files omitted at file limit");
                            current.totals_exact = false;
                            current.file_count_exact = false;
                            break;
                        }
                        flush_file(&mut files, &mut file, &mut hunk)
                    }
                    file.get_or_insert_with(empty_file).old_path = parse_path(path);
                    git_header_awaits_file_headers = false;
                    continue;
                }
                if let Some(path) = line.strip_prefix("+++ ") {
                    file.get_or_insert_with(empty_file).new_path = parse_path(path);
                    continue;
                }
            }
            if let Some(path) = line.strip_prefix("rename from ") {
                file.get_or_insert_with(empty_file).old_path = parse_path(path);
                continue;
            }
            if let Some(path) = line.strip_prefix("rename to ") {
                file.get_or_insert_with(empty_file).new_path = parse_path(path);
                continue;
            }
            if let Some((old, new)) = parse_binary_paths(line) {
                let f = file.get_or_insert_with(empty_file);
                if f.old_path.is_empty() {
                    f.old_path = old;
                }
                if f.new_path.is_empty() {
                    f.new_path = new;
                }
                f.binary = true;
                f.record_elision("binary content omitted");
                continue;
            }
            if line == "GIT binary patch" {
                let f = file.get_or_insert_with(empty_file);
                f.binary = true;
                f.record_elision("binary content omitted");
                continue;
            }
            if line.starts_with("@@") {
                if accepted_hunks >= MAX_DIFF_HUNKS {
                    let current = file.get_or_insert_with(empty_file);
                    current.record_elision("diff exceeded hunk limit; later files may be omitted");
                    current.totals_exact = false;
                    current.file_count_exact = false;
                    break;
                }
                flush_hunk(&mut file, &mut hunk);
                hunk = parse_hunk_header(line);
                if hunk.is_some() {
                    accepted_hunks += 1;
                }
                continue;
            }
            if let Some(note) = line.strip_prefix("# finch: ") {
                let current = file.get_or_insert_with(empty_file);
                current.record_elision(sanitize_terminal(note));
                // Canonical Finch markers describe presentation that was
                // omitted before this payload reached the replay parser. The
                // retained counts can therefore only be lower bounds.
                current.totals_exact = false;
                current.file_count_exact = false;
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
                if let Some(current) = file.as_mut() {
                    match kind {
                        DiffLineKind::Add => {
                            current.total_added = current.total_added.saturating_add(1)
                        }
                        DiffLineKind::Remove => {
                            current.total_removed = current.total_removed.saturating_add(1)
                        }
                        _ => {}
                    }
                }
                if accepted_lines < MAX_DIFF_LINES {
                    let (text, line_elided) = sanitize_diff_line(value);
                    h.lines.push(DiffLine { kind, text });
                    accepted_lines += 1;
                    if line_elided {
                        let current = file.get_or_insert_with(empty_file);
                        current.record_elision("one or more diff lines truncated");
                    }
                } else {
                    let current = file.get_or_insert_with(empty_file);
                    current.record_elision(format!("diff truncated at {MAX_DIFF_LINES} lines"));
                    current.totals_exact = false;
                }
            }
        }
        flush_file(&mut files, &mut file, &mut hunk);
        for parsed in &mut files {
            parsed.mark_truncated_rendering();
        }
        files
    }

    /// Parse a line-oriented retained payload without first joining an
    /// unbounded vector supplied by a reconnect or replay path.
    pub fn parse_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Self> {
        let mut source = String::new();
        for (index, line) in lines.into_iter().enumerate() {
            if index >= MAX_DIFF_STRUCTURAL_LINES {
                return vec![elided_input("diff exceeded structural line limit")];
            }
            let separator_bytes = usize::from(index > 0);
            let Some(required) = source
                .len()
                .checked_add(separator_bytes)
                .and_then(|size| size.checked_add(line.len()))
            else {
                return vec![elided_input("diff input exceeded display byte limit")];
            };
            if required > MAX_DIFF_INPUT_BYTES {
                return vec![elided_input("diff input exceeded display byte limit")];
            }
            if index > 0 {
                source.push('\n');
            }
            source.push_str(line);
        }
        Self::parse_all(&source)
    }

    pub fn to_unified(&self) -> String {
        self.unified_text().text
    }
    pub fn added(&self) -> usize {
        self.total_added
    }
    pub fn removed(&self) -> usize {
        self.total_removed
    }
    /// Whether retained added/removed counts are exact, including that
    /// canonical [`Self::to_unified`] rendering was not cut at
    /// [`MAX_RENDER_CHARS`].
    ///
    /// A silent render bound used to leave this true while added lines were
    /// missing from the string. Ask [`Self::is_complete`] when the question
    /// is whether the whole view is faithful.
    pub fn counts_are_exact(&self) -> bool {
        self.totals_exact && !self.unified_text().truncated
    }
    /// Whether the number of files in this payload is exact, or only a
    /// lower bound because later files were omitted at a parse limit.
    pub fn file_count_is_exact(&self) -> bool {
        self.file_count_exact
    }
    /// Honest answer to "is this complete?": exact line counts, exact file
    /// count, no content-omitting elision, and [`Self::to_unified`] would not
    /// hit [`MAX_RENDER_CHARS`]. Informational line-ending notes do not fail
    /// this check.
    pub(crate) fn is_complete(&self) -> bool {
        self.counts_are_exact()
            && self.file_count_is_exact()
            && match &self.elided {
                None => true,
                Some(note) => elision_is_only_line_ending_note(note),
            }
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
    pub fn is_created(&self) -> bool {
        self.old_path == "/dev/null" && self.new_path != "/dev/null"
    }
    pub fn render(&self, colors: &ColorScheme, mode: DiffColorMode) -> String {
        let meta = if self.is_rename() {
            format!("{} → {}", self.old_path, self.new_path)
        } else {
            self.display_path().into()
        };
        let mut header = format!(
            "{}  +{} -{}",
            sanitize_terminal(&meta),
            self.count_label(self.added()),
            self.count_label(self.removed())
        );
        if self.binary {
            header.push_str("  binary")
        }
        if self.is_created() {
            header.push_str("  created")
        } else if self.is_rename() {
            header.push_str("  renamed")
        }
        if let Some(v) = &self.elided {
            header.push_str(&format!("  [{}]", sanitize_terminal(v)))
        }
        let mut out = paint(header, colors, mode, Tone::Meta);
        let width = self
            .hunks
            .iter()
            .filter_map(|h| {
                Some(
                    h.old_start
                        .checked_add(h.old_count)?
                        .max(h.new_start.checked_add(h.new_count)?),
                )
            })
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
                        old = old.saturating_add(1);
                        new = new.saturating_add(1);
                        v
                    }
                    DiffLineKind::Remove => {
                        let v = (Some(old), None, '-', Tone::Remove);
                        old = old.saturating_add(1);
                        v
                    }
                    DiffLineKind::Add => {
                        let v = (None, Some(new), '+', Tone::Add);
                        new = new.saturating_add(1);
                        v
                    }
                    DiffLineKind::NoNewline => (None, None, '\\', Tone::Meta),
                };
                out.push('\n');
                out.push_str(&paint(
                    format_gutter_line(a, b, m, &sanitize_terminal(&line.text), width),
                    colors,
                    mode,
                    t,
                ))
            }
        }
        bound_rendered(out, "… [diff rendering truncated]").text
    }

    fn count_label(&self, value: usize) -> String {
        if self.totals_exact {
            value.to_string()
        } else {
            format!("≥{value}")
        }
    }

    fn ingest_similar(&mut self, text_diff: &TextDiff<'_, '_, '_, str>) {
        let mut accepted_lines = 0usize;
        for ops in text_diff.grouped_ops(3) {
            if ops.is_empty() {
                continue;
            }
            if self.hunks.len() >= MAX_DIFF_HUNKS {
                self.record_elision("diff exceeded hunk limit; later hunks omitted");
                self.totals_exact = false;
                break;
            }
            let mut hunk = hunk_from_ops(&ops);
            for op in &ops {
                for change in text_diff.iter_changes(op) {
                    let kind = match change.tag() {
                        ChangeTag::Equal => DiffLineKind::Context,
                        ChangeTag::Delete => DiffLineKind::Remove,
                        ChangeTag::Insert => DiffLineKind::Add,
                    };
                    let value = change.value().trim_end_matches(['\n', '\r']);
                    self.push_hunk_line(&mut hunk, &mut accepted_lines, kind, value);
                    if change.missing_newline() {
                        self.push_hunk_line(
                            &mut hunk,
                            &mut accepted_lines,
                            DiffLineKind::NoNewline,
                            "No newline at end of file",
                        );
                    }
                }
            }
            self.hunks.push(hunk);
        }
    }

    fn push_hunk_line(
        &mut self,
        hunk: &mut DiffHunk,
        accepted_lines: &mut usize,
        kind: DiffLineKind,
        value: &str,
    ) {
        match kind {
            DiffLineKind::Add => self.total_added = self.total_added.saturating_add(1),
            DiffLineKind::Remove => self.total_removed = self.total_removed.saturating_add(1),
            _ => {}
        }
        if *accepted_lines < MAX_DIFF_LINES {
            let (text, line_elided) = sanitize_diff_line(value);
            hunk.lines.push(DiffLine { kind, text });
            *accepted_lines += 1;
            if line_elided {
                self.record_elision("one or more diff lines truncated");
            }
        } else {
            self.record_elision(format!("diff truncated at {MAX_DIFF_LINES} lines"));
            self.totals_exact = false;
        }
    }

    fn record_elision(&mut self, note: impl Into<String>) {
        let note = note.into();
        match &mut self.elided {
            Some(existing) if existing.split("; ").any(|part| part == note) => {}
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(&note);
            }
            None => self.elided = Some(note),
        }
    }

    fn mark_truncated_rendering(&mut self) {
        if self.unified_text().truncated {
            self.record_elision("diff rendering truncated");
            self.totals_exact = false;
        }
    }

    fn unified_text(&self) -> BoundedRender {
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
            return BoundedRender {
                text: out,
                truncated: false,
            };
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
        bound_rendered(out, "# finch: diff rendering truncated")
    }
}

/// Build the stable path and aggregate line-count summary for parsed files.
pub fn summarize_files(files: &[FileDiff]) -> String {
    let added: usize = files.iter().map(FileDiff::added).sum();
    let removed: usize = files.iter().map(FileDiff::removed).sum();
    let exact = files.iter().all(FileDiff::counts_are_exact);
    let added = if exact {
        added.to_string()
    } else {
        format!("≥{added}")
    };
    let removed = if exact {
        removed.to_string()
    } else {
        format!("≥{removed}")
    };
    if files.len() == 1 {
        format!("{}  +{} -{}", files[0].display_path(), added, removed)
    } else {
        let file_count = if files.iter().all(FileDiff::file_count_is_exact) {
            files.len().to_string()
        } else {
            format!("≥{}", files.len())
        };
        format!("{file_count} files  +{} -{}", added, removed)
    }
}

/// Render one bounded changeset using a single theme and total output limit.
pub fn render_files(files: &[FileDiff], colors: &ColorScheme, mode: DiffColorMode) -> String {
    let mut rendered = files
        .iter()
        .map(|diff| diff.render(colors, mode))
        .collect::<Vec<_>>();
    if files.len() > 1 {
        rendered.insert(0, summarize_files(files));
    }
    bound_rendered(rendered.join("\n"), "… [diff rendering truncated]").text
}

fn empty_file() -> FileDiff {
    FileDiff {
        old_path: String::new(),
        new_path: String::new(),
        binary: false,
        elided: None,
        hunks: vec![],
        total_added: 0,
        total_removed: 0,
        totals_exact: true,
        file_count_exact: true,
    }
}

fn elided_input(reason: &str) -> FileDiff {
    FileDiff {
        old_path: "(diff input)".into(),
        new_path: "(diff input)".into(),
        binary: false,
        elided: Some(reason.into()),
        hunks: vec![],
        total_added: 0,
        total_removed: 0,
        totals_exact: false,
        file_count_exact: false,
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
fn counted_hunk_lines(hunk: &DiffHunk) -> (usize, usize) {
    let mut old = 0usize;
    let mut new = 0usize;
    for line in &hunk.lines {
        match line.kind {
            DiffLineKind::Context => {
                old = old.saturating_add(1);
                new = new.saturating_add(1);
            }
            DiffLineKind::Remove => old = old.saturating_add(1),
            DiffLineKind::Add => new = new.saturating_add(1),
            DiffLineKind::NoNewline => {}
        }
    }
    (old, new)
}

fn hunk_body_is_open(hunk: &DiffHunk) -> bool {
    let (old, new) = counted_hunk_lines(hunk);
    old < hunk.old_count || new < hunk.new_count
}

fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let end = line.get(2..)?.find("@@")? + 2;
    let mut p = line.get(2..end)?.split_whitespace();
    let (old_start, old_count) = range(p.next()?)?;
    let (new_start, new_count) = range(p.next()?)?;
    old_start.checked_add(old_count)?;
    new_start.checked_add(new_count)?;
    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        context: sanitize_terminal(line.get(end + 2..).unwrap_or("")),
        lines: vec![],
    })
}
fn parse_git_paths(value: &str) -> Option<(String, String)> {
    let tokens = git_tokens(value);
    Some((parse_path(tokens.first()?), parse_path(tokens.get(1)?)))
}
fn git_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push('\\');
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quoted {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            current.push(ch);
            continue;
        }
        if ch.is_whitespace() && !quoted {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
fn parse_binary_paths(line: &str) -> Option<(String, String)> {
    let middle = line
        .strip_prefix("Binary files ")?
        .strip_suffix(" differ")?;
    let (old, new) = middle.split_once(" and ")?;
    Some((parse_path(old), parse_path(new)))
}
struct BoundedRender {
    text: String,
    truncated: bool,
}

fn bound_rendered(value: String, marker: &str) -> BoundedRender {
    if value.chars().count() <= MAX_RENDER_CHARS {
        return BoundedRender {
            text: value,
            truncated: false,
        };
    }
    let prefix: String = value.chars().take(MAX_RENDER_CHARS).collect();
    let cut = prefix.rfind('\n').unwrap_or(prefix.len());
    BoundedRender {
        text: format!("{}\n{marker}", &prefix[..cut]),
        truncated: true,
    }
}

fn elision_is_only_line_ending_note(note: &str) -> bool {
    note.starts_with("line endings ") && !note.contains(';')
}

fn hunk_from_ops(ops: &[similar::DiffOp]) -> DiffHunk {
    let (first, last) = match (ops.first(), ops.last()) {
        (Some(first), Some(last)) => (first, last),
        _ => {
            return DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 0,
                new_count: 0,
                context: String::new(),
                lines: vec![],
            };
        }
    };
    let old_start_0 = first.old_range().start;
    let old_count = last.old_range().end.saturating_sub(old_start_0);
    let new_start_0 = first.new_range().start;
    let new_count = last.new_range().end.saturating_sub(new_start_0);
    DiffHunk {
        old_start: if old_count == 0 {
            old_start_0
        } else {
            old_start_0.saturating_add(1)
        },
        old_count,
        new_start: if new_count == 0 {
            new_start_0
        } else {
            new_start_0.saturating_add(1)
        },
        new_count,
        context: String::new(),
        lines: vec![],
    }
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
    let decoded = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
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
    sanitize_terminal_bounded(s, "…").0
}

fn sanitize_diff_line(s: &str) -> (String, bool) {
    sanitize_terminal_bounded(s, "… [line truncated]")
}

fn sanitize_terminal_bounded(s: &str, marker: &str) -> (String, bool) {
    let marker_len = marker.chars().count();
    let content_limit = MAX_DIFF_LINE_CHARS.saturating_sub(marker_len);
    let mut out = String::with_capacity(s.len().min(MAX_DIFF_LINE_CHARS));
    let mut visible = 0usize;
    let mut truncated = false;
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
        let replacement = match c {
            '\t' => "    ".to_string(),
            c if c.is_control() => "�".to_string(),
            c => c.to_string(),
        };
        let replacement_len = replacement.chars().count();
        if visible.saturating_add(replacement_len) > MAX_DIFF_LINE_CHARS {
            truncated = true;
            continue;
        }
        out.push_str(&replacement);
        visible = visible.saturating_add(replacement_len);
    }
    if truncated {
        out = out.chars().take(content_limit).collect();
        out.push_str(marker);
    }
    (out, truncated)
}
/// Remove terminal controls from bounded multi-line dialog content.
pub fn sanitize_multiline(s: &str) -> String {
    const MARKER: &str = "… [content truncated]";
    if s.len() > MAX_DIFF_INPUT_BYTES {
        return MARKER.into();
    }
    let mut out = String::new();
    let mut chars = 0usize;
    for (index, line) in s.lines().enumerate() {
        if index >= MAX_DIFF_STRUCTURAL_LINES {
            out.push('\n');
            out.push_str(MARKER);
            return out;
        }
        let clean = sanitize_terminal(line);
        let separator = usize::from(index > 0);
        let next = chars
            .saturating_add(separator)
            .saturating_add(clean.chars().count());
        if next > MAX_RENDER_CHARS {
            out.push('\n');
            out.push_str(MARKER);
            return out;
        }
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&clean);
        chars = next;
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

/// One right-aligned number, then the marker. Context uses the new-file
/// number; add uses new; remove uses old. Two side-by-side numbers duplicated
/// the same logical line and put 13 in a different column on `-` vs `+`.
fn format_gutter_line(
    old: Option<usize>,
    new: Option<usize>,
    marker: char,
    text: &str,
    width: usize,
) -> String {
    match new.or(old) {
        Some(number) => format!("{number:>width$} {marker} {text}"),
        None => format!("{:width$} {marker} {text}", ""),
    }
}
fn scheme_is_dark(colors: &ColorScheme) -> bool {
    matches!(
        colors.message_band_style(MessageBand::Tool).fg,
        Some(Color::Rgb(r, g, b))
            if (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 127
    )
}

/// Foreground and background for one diff tone. The background carries add /
/// remove / context; the foreground is a contrasting readable ink, never the
/// same near-white as an unpainted dialog or terminal default.
fn tone_colors(dark: bool, tone: Tone) -> ((u8, u8, u8), (u8, u8, u8)) {
    match (dark, tone) {
        (true, Tone::Add) => ((236, 246, 238), (20, 72, 40)),
        (true, Tone::Remove) => ((255, 236, 236), (88, 24, 28)),
        (true, Tone::Hunk) => ((186, 214, 255), (28, 42, 64)),
        (true, Tone::Meta) => ((210, 214, 220), (52, 56, 62)),
        (true, Tone::Context) => ((220, 224, 230), (44, 48, 54)),
        (false, Tone::Add) => ((12, 56, 28), (204, 240, 214)),
        (false, Tone::Remove) => ((112, 16, 22), (255, 214, 214)),
        (false, Tone::Hunk) => ((12, 48, 112), (220, 232, 250)),
        (false, Tone::Meta) => ((48, 52, 58), (232, 234, 236)),
        (false, Tone::Context) => ((28, 32, 38), (234, 236, 238)),
    }
}

fn paint(text: String, colors: &ColorScheme, mode: DiffColorMode, tone: Tone) -> String {
    if mode == DiffColorMode::NoColor {
        return text;
    }
    let (fg, bg) = tone_colors(scheme_is_dark(colors), tone);
    format!(
        "\x1b[38;2;{};{};{};48;2;{};{};{}m{text}\x1b[0m",
        fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use finch_theme::ColorTheme;
    const SAMPLE: &str =
        "--- a/src/old.rs\n+++ b/src/new.rs\n@@ -2,2 +2,3 @@ fn x\n keep\n-old\n+new\n+more\n";
    #[test]
    fn no_color_snapshot() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        assert_eq!(d.render(&ColorScheme::default(),DiffColorMode::NoColor),"src/old.rs → src/new.rs  +2 -1  renamed\n@@ -2,2 +2,3 @@ fn x\n2   keep\n3 - old\n3 + new\n4 + more")
    }

    fn hunk_body(rendered: &str) -> Vec<&str> {
        rendered
            .lines()
            .skip_while(|line| !line.starts_with("@@"))
            .skip(1)
            .collect()
    }

    fn first_number_column(line: &str) -> usize {
        line.find(|c: char| c.is_ascii_digit())
            .unwrap_or_else(|| panic!("gutter line must carry a line number, got {line:?}"))
    }

    #[test]
    fn test_diff_gutter_does_not_duplicate_line_numbers() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        let rendered = d.render(&ColorScheme::default(), DiffColorMode::NoColor);
        let body = hunk_body(&rendered);
        assert!(!body.is_empty(), "expected hunk body lines in {rendered}");

        let keep = body
            .iter()
            .find(|line| line.contains("keep"))
            .unwrap_or_else(|| panic!("context line missing from {rendered}"));
        assert_eq!(
            keep.matches('2').count(),
            1,
            "context gutter must show the line number once, not duplicated side-by-side; line={keep:?} rendered={rendered}"
        );

        let removed = body
            .iter()
            .find(|line| line.contains("old"))
            .unwrap_or_else(|| panic!("remove line missing from {rendered}"));
        let added = body
            .iter()
            .find(|line| line.contains("new"))
            .unwrap_or_else(|| panic!("add line missing from {rendered}"));
        assert_eq!(
            first_number_column(removed),
            first_number_column(added),
            "the same logical number must occupy the same gutter column on every row type; removed={removed:?} added={added:?} rendered={rendered}"
        );

        let old: String = (1..=16).map(|i| format!("line {i}\n")).collect();
        let new: String = (1..=16)
            .map(|i| {
                if i == 13 || i == 14 {
                    format!("changed {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let two_digit = FileDiff::from_texts("index.html", &old, &new)
            .render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(
            !two_digit.contains("15 15"),
            "context rows must not duplicate the line number; rendered={two_digit}"
        );
        let two_body = hunk_body(&two_digit);
        let two_removed = two_body
            .iter()
            .find(|line| line.contains("line 13") && line.contains('-'))
            .unwrap_or_else(|| panic!("remove of line 13 missing from {two_digit}"));
        let two_added = two_body
            .iter()
            .find(|line| line.contains("changed 13"))
            .unwrap_or_else(|| panic!("add of line 13 missing from {two_digit}"));
        assert_eq!(
            first_number_column(two_removed),
            first_number_column(two_added),
            "two-digit gutters must put 13 in the same column on remove and add; removed={two_removed:?} added={two_added:?} rendered={two_digit}"
        );
        let context_15 = two_body
            .iter()
            .find(|line| line.contains("line 15"))
            .unwrap_or_else(|| panic!("context line 15 missing from {two_digit}"));
        let gutter = context_15
            .split_once("line 15")
            .map(|(prefix, _)| prefix)
            .unwrap_or_else(|| panic!("context line 15 missing payload in {context_15:?}"));
        assert_eq!(
            gutter.matches("15").count(),
            1,
            "context gutter must not print 15 twice; gutter={gutter:?} line={context_15:?} rendered={two_digit}"
        );
    }
    #[test]
    fn light_dark() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        assert_ne!(
            d.render(&ColorTheme::Dark.to_scheme(), DiffColorMode::Theme),
            d.render(&ColorTheme::Light.to_scheme(), DiffColorMode::Theme)
        )
    }

    fn sgr_triplet(kind: u8, rgb: (u8, u8, u8)) -> String {
        format!("{kind};2;{};{};{}", rgb.0, rgb.1, rgb.2)
    }

    #[test]
    fn themed_diff_fills_rows_with_backgrounds_not_foreground_only() {
        let d = FileDiff::parse(SAMPLE).unwrap();
        let dark = d.render(&ColorTheme::Dark.to_scheme(), DiffColorMode::Theme);
        let light = d.render(&ColorTheme::Light.to_scheme(), DiffColorMode::Theme);
        let (add_fg, add_bg) = tone_colors(true, Tone::Add);
        let (remove_fg, remove_bg) = tone_colors(true, Tone::Remove);
        let (context_fg, context_bg) = tone_colors(true, Tone::Context);
        let (meta_fg, meta_bg) = tone_colors(true, Tone::Meta);
        for (label, rendered, fg, bg) in [
            ("add", &dark, add_fg, add_bg),
            ("remove", &dark, remove_fg, remove_bg),
            ("context", &dark, context_fg, context_bg),
            ("header", &dark, meta_fg, meta_bg),
        ] {
            assert!(
                rendered.contains(&sgr_triplet(38, fg)) && rendered.contains(&sgr_triplet(48, bg)),
                "{label} must set contrasting ink and a filled background; rendered={rendered}"
            );
            let fg_luma = (fg.0 as u32 * 299 + fg.1 as u32 * 587 + fg.2 as u32 * 114) / 1000;
            let bg_luma = (bg.0 as u32 * 299 + bg.1 as u32 * 587 + bg.2 as u32 * 114) / 1000;
            assert!(
                fg_luma.abs_diff(bg_luma) >= 80,
                "{label} fg {fg:?} vs bg {bg:?} is too close (white-on-white / black-on-black)"
            );
        }
        let header = dark.lines().next().expect("header row");
        assert!(
            header.contains(&sgr_triplet(48, meta_bg)),
            "the top of the diff must carry the meta background, not unpainted default; header={header:?}"
        );
        assert!(
            light.contains(&sgr_triplet(48, tone_colors(false, Tone::Add).1)),
            "light theme must also fill add rows; light={light}"
        );
        assert!(
            !dark.contains("38;2;245;247;250m") || dark.contains("48;2;"),
            "near-white context ink without a fill is the reported white-on-white header/footer"
        );
    }
    #[test]
    fn production_mode_honors_accessible_no_color_environment() {
        assert_eq!(
            DiffColorMode::for_environment(true, Some("xterm-256color")),
            DiffColorMode::NoColor
        );
        assert_eq!(
            DiffColorMode::for_environment(false, Some("dumb")),
            DiffColorMode::NoColor
        );
        assert_eq!(
            DiffColorMode::for_environment(false, Some("xterm-256color")),
            DiffColorMode::Theme
        );
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
    fn malformed_single_quote_path_does_not_panic() {
        let diff = FileDiff::parse("--- \"\n+++ b/safe\n").unwrap();
        assert_eq!(diff.old_path, "\"");
        assert_eq!(diff.new_path, "safe");
    }
    #[test]
    fn large_diff_is_elided() {
        let x = "a".repeat(MAX_DIFF_INPUT_BYTES);
        assert!(FileDiff::from_texts("x", &x, &x).elided.is_some())
    }

    #[test]
    fn external_diff_input_and_line_are_bounded() {
        let hostile = format!(
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-{}\n+ok\n",
            "z".repeat(MAX_DIFF_INPUT_BYTES)
        );
        let parsed = FileDiff::parse(&hostile).unwrap();
        assert!(parsed.elided.is_some());
        assert!(parsed
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .all(|line| { line.text.chars().count() <= MAX_DIFF_LINE_CHARS }));
    }

    #[test]
    fn newline_only_oversized_input_is_rejected_before_traversal() {
        let parsed = FileDiff::parse(&"\n".repeat(MAX_DIFF_INPUT_BYTES + 1)).unwrap();
        assert!(parsed.elided.as_deref().unwrap().contains("byte limit"));
        assert!(parsed.hunks.is_empty());
        assert_eq!(
            sanitize_multiline(&"\n".repeat(MAX_DIFF_INPUT_BYTES + 1)),
            "… [content truncated]"
        );
    }

    #[test]
    fn hostile_hunk_count_and_usize_ranges_are_bounded() {
        let headers = format!("--- a/x\n+++ b/x\n{}", "@@ -1,0 +1,0 @@\n".repeat(10_000));
        let parsed = FileDiff::parse(&headers).unwrap();
        assert!(parsed.hunks.len() <= MAX_DIFF_HUNKS);
        let impossible = format!("--- a/x\n+++ b/x\n@@ -{},2 +1,1 @@\n-x\n+y\n", usize::MAX);
        assert!(FileDiff::parse(&impossible).unwrap().hunks.is_empty());

        let files = (0..100)
            .map(|index| format!("diff --git a/{index} b/{index}\n"))
            .collect::<String>();
        let parsed = FileDiff::parse_all(&files);
        assert_eq!(parsed.len(), MAX_DIFF_FILES);
        assert_eq!(summarize_files(&parsed), "≥64 files  +≥0 -≥0");
        assert!(
            render_files(&parsed, &ColorScheme::default(), DiffColorMode::NoColor)
                .contains("additional files omitted")
        );
    }

    #[test]
    fn hunk_limit_marks_aggregate_file_and_line_counts_as_lower_bounds() {
        let patch = format!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n\
             diff --git a/b b/b\n--- a/b\n+++ b/b\n{}",
            "@@ -1 +1 @@\n-x\n+y\n".repeat(MAX_DIFF_HUNKS)
        );
        let files = FileDiff::parse_all(&patch);
        assert_eq!(files.len(), 2);
        assert!(summarize_files(&files).starts_with("≥2 files  +≥"));
        let rendered = render_files(&files, &ColorScheme::default(), DiffColorMode::NoColor);
        assert!(
            rendered.contains("later files may be omitted"),
            "{rendered}"
        );
    }

    #[test]
    fn structural_limit_marks_aggregate_file_and_line_counts_as_lower_bounds() {
        let patch = format!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n\
             diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1,2000 +1,2000 @@\n{}",
            " same\n".repeat(MAX_DIFF_STRUCTURAL_LINES)
        );
        let files = FileDiff::parse_all(&patch);
        assert_eq!(files.len(), 2);
        assert!(summarize_files(&files).starts_with("≥2 files  +≥"));
        let rendered = render_files(&files, &ColorScheme::default(), DiffColorMode::NoColor);
        assert!(
            rendered.contains("structural line limit; later files may be omitted"),
            "{rendered}"
        );
    }

    #[test]
    fn standard_git_binary_diff_attributes_paths_without_file_headers() {
        let parsed = FileDiff::parse(
            "diff --git a/old.bin b/new.bin\nBinary files a/old.bin and b/new.bin differ\n",
        )
        .unwrap();
        assert_eq!(
            (parsed.old_path.as_str(), parsed.new_path.as_str()),
            ("old.bin", "new.bin")
        );
        assert!(parsed.binary);
    }

    #[test]
    fn canonical_multi_file_payload_keeps_leading_binary_file() {
        let binary = FileDiff::from_texts("image.bin", "", "\0").to_unified();
        let text = FileDiff::from_texts("notes.txt", "old\n", "new\n").to_unified();
        let parsed = FileDiff::parse_all(&format!("{binary}{text}"));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].display_path(), "image.bin");
        assert!(parsed[0].binary);
        assert_eq!(parsed[1].display_path(), "notes.txt");
    }

    #[test]
    fn rendered_output_has_a_total_character_bound() {
        let body = "x".repeat(MAX_DIFF_LINE_CHARS);
        let patch = format!(
            "--- a/x\n+++ b/x\n@@ -1,400 +1,400 @@\n{}",
            (0..400)
                .map(|_| format!("-{body}\n+{body}\n"))
                .collect::<String>()
        );
        let rendered = FileDiff::parse(&patch)
            .unwrap()
            .render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(rendered.chars().count() <= MAX_RENDER_CHARS + 40);
        assert!(rendered.contains("rendering truncated"));
    }

    #[test]
    fn aggregate_render_has_one_total_character_bound() {
        let line = "x".repeat(MAX_DIFF_LINE_CHARS);
        let contents = format!("{}\n", line).repeat(16);
        let files = (0..MAX_DIFF_FILES)
            .map(|index| FileDiff::from_texts(&format!("{index}.txt"), "", &contents))
            .collect::<Vec<_>>();
        let rendered = render_files(&files, &ColorScheme::default(), DiffColorMode::NoColor);
        assert!(rendered.chars().count() <= MAX_RENDER_CHARS + 40);
        assert!(rendered.contains("rendering truncated"));
    }

    #[test]
    fn totals_remain_truthful_beyond_the_transcript_preview() {
        let patch = format!(
            "--- a/x\n+++ b/x\n@@ -1,500 +1,500 @@\n{}",
            (0..500)
                .map(|index| format!("-old {index}\n+new {index}\n"))
                .collect::<String>()
        );
        let parsed = FileDiff::parse(&patch).unwrap();
        assert_eq!((parsed.added(), parsed.removed()), (500, 500));
        assert!(parsed.counts_are_exact());
        assert!(parsed.hunks[0].lines.len() > MAX_DIFF_PREVIEW_LINES);
    }

    #[test]
    fn generated_diff_is_rejected_before_myers_above_line_ceiling() {
        let old = "old\n".repeat(MAX_DIFF_COMPUTE_LINES / 2 + 1);
        let new = "new\n".repeat(MAX_DIFF_COMPUTE_LINES / 2 + 1);
        let diff = FileDiff::from_texts("large.txt", &old, &new);
        assert!(diff.hunks.is_empty());
        assert!(!diff.counts_are_exact());
        assert!(diff
            .elided
            .as_deref()
            .unwrap()
            .contains("computation limit"));
    }

    #[test]
    fn long_diff_line_retains_visible_tail_elision_state() {
        let patch = format!("--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+{}\n", "x".repeat(513));
        let diff = FileDiff::parse(&patch).unwrap();
        assert_eq!(
            diff.hunks[0].lines[0].text.chars().count(),
            MAX_DIFF_LINE_CHARS
        );
        assert!(diff.hunks[0].lines[0].text.ends_with("… [line truncated]"));
        assert_eq!(
            diff.elided.as_deref(),
            Some("one or more diff lines truncated")
        );
        assert!(diff
            .render(&ColorScheme::default(), DiffColorMode::NoColor)
            .contains("… [line truncated]"));
    }

    #[test]
    fn created_file_roundtrip_preserves_dev_null_and_created_metadata() {
        let created = FileDiff::from_created("new.txt", "hello\n");
        assert_eq!(created.old_path, "/dev/null");
        assert!(created.is_created());
        let rendered = created.render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(rendered.contains("created"), "{rendered}");
        assert!(!rendered.contains("renamed"), "{rendered}");

        let replayed = FileDiff::parse(&created.to_unified()).unwrap();
        assert_eq!(replayed.old_path, "/dev/null");
        assert!(replayed.is_created());
    }

    #[test]
    fn bounded_input_labels_totals_as_lower_bounds() {
        let parsed = FileDiff::parse(&"\n".repeat(MAX_DIFF_INPUT_BYTES + 1)).unwrap();
        assert!(!parsed.counts_are_exact());
        assert!(parsed
            .render(&ColorScheme::default(), DiffColorMode::NoColor)
            .contains("+≥0 -≥0"));
    }

    #[test]
    fn inexact_totals_survive_canonical_roundtrip() {
        let bounded = FileDiff::from_texts("x", &"a".repeat(MAX_DIFF_INPUT_BYTES), "new");
        assert!(!bounded.counts_are_exact());

        let replayed = FileDiff::parse(&bounded.to_unified()).unwrap();
        assert!(!replayed.counts_are_exact());
        assert!(replayed
            .render(&ColorScheme::default(), DiffColorMode::NoColor)
            .contains("+≥0 -≥0"));
    }

    #[test]
    fn test_from_texts_keeps_removed_double_dash_and_added_double_plus_lines() {
        let old = "SELECT 1;\n-- keep totals\n-- and averages\nSELECT 2;\n";
        let new = "SELECT 1;\n++ keep totals\n++ and averages\nSELECT 2;\n";
        let diff = FileDiff::from_texts("q.sql", old, new);
        let lines_of = |kind: DiffLineKind| -> Vec<&str> {
            diff.hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == kind)
                .map(|line| line.text.as_str())
                .collect()
        };
        let removed = lines_of(DiffLineKind::Remove);
        let added = lines_of(DiffLineKind::Add);
        assert!(
            removed.contains(&"-- keep totals") && removed.contains(&"-- and averages"),
            "removed lines beginning '-- ' must appear in the FileDiff; round-trip parse would \
             treat them as a file header and drop the change. added={} removed={} hunks={:?}",
            diff.added(),
            diff.removed(),
            diff.hunks
        );
        assert!(
            added.contains(&"++ keep totals") && added.contains(&"++ and averages"),
            "added lines beginning '++ ' must appear in the FileDiff; round-trip parse would \
             treat them as a file header and drop the change. added={} removed={} hunks={:?}",
            diff.added(),
            diff.removed(),
            diff.hunks
        );
        assert_eq!(
            (diff.added(), diff.removed()),
            (2, 2),
            "header-looking lines must be counted; added={} removed={} exact={} hunks={:?}",
            diff.added(),
            diff.removed(),
            diff.counts_are_exact(),
            diff.hunks
        );
        assert!(
            diff.counts_are_exact() && diff.is_complete(),
            "a four-line SQL replacement must stay complete after keeping header-looking lines; \
             elided={:?}",
            diff.elided
        );
        let rendered = diff.render(&ColorScheme::default(), DiffColorMode::NoColor);
        assert!(
            rendered.contains("-- keep totals") && rendered.contains("++ keep totals"),
            "render must show the header-looking lines that re-parsing a unified string would \
             drop; got {rendered}"
        );
    }

    #[test]
    fn test_parse_all_keeps_in_hunk_double_dash_and_double_plus_lines() {
        let old = "SELECT 1;\n-- keep totals\n-- and averages\nSELECT 2;\n";
        let new = "SELECT 1;\n++ keep totals\n++ and averages\nSELECT 2;\n";
        let unified = FileDiff::from_texts("q.sql", old, new).to_unified();
        assert!(
            unified.contains("--- keep totals") && unified.contains("+++ keep totals"),
            "fixture must serialize the header-looking lines so parse_all can drop them; got {unified}"
        );

        let files = FileDiff::parse_all(&unified);
        assert_eq!(
            files.len(),
            1,
            "in-hunk '--- keep totals' must not start a second file; restoring header-before-hunk \
             dispatch splits the change and drops the removal. files={files:?} unified={unified}"
        );
        let parsed = FileDiff::parse(&unified).unwrap_or_else(|| {
            panic!("parse must retain the SQL file; unified={unified}");
        });
        let lines_of = |kind: DiffLineKind| -> Vec<&str> {
            parsed
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == kind)
                .map(|line| line.text.as_str())
                .collect()
        };
        let removed = lines_of(DiffLineKind::Remove);
        let added = lines_of(DiffLineKind::Add);
        assert!(
            removed.contains(&"-- keep totals") && removed.contains(&"-- and averages"),
            "parse(from_texts().to_unified()) must keep removed '-- ' lines; header-before-hunk \
             dispatch treats '--- keep totals' as a file header and drops the change. \
             added={} removed={} exact={} complete={} files={} hunks={:?} unified={unified}",
            parsed.added(),
            parsed.removed(),
            parsed.counts_are_exact(),
            parsed.is_complete(),
            files.len(),
            parsed.hunks
        );
        assert!(
            added.contains(&"++ keep totals") && added.contains(&"++ and averages"),
            "parse(from_texts().to_unified()) must keep added '++ ' lines; header-before-hunk \
             dispatch treats '+++ keep totals' as a file header and drops the change. \
             added={} removed={} exact={} complete={} files={} hunks={:?} unified={unified}",
            parsed.added(),
            parsed.removed(),
            parsed.counts_are_exact(),
            parsed.is_complete(),
            files.len(),
            parsed.hunks
        );
        assert_eq!(
            (parsed.added(), parsed.removed()),
            (2, 2),
            "replayed counts must include the header-looking lines; added={} removed={} \
             exact={} complete={} hunks={:?}",
            parsed.added(),
            parsed.removed(),
            parsed.counts_are_exact(),
            parsed.is_complete(),
            parsed.hunks
        );
        assert!(
            parsed.counts_are_exact() && parsed.file_count_is_exact() && parsed.is_complete(),
            "keeping the lines must remain an exact complete view, not a remnant that claims \
             exactness after dropping them; elided={:?} files={}",
            parsed.elided,
            files.len()
        );

        let from_lines = FileDiff::parse_lines(unified.lines());
        assert_eq!(
            from_lines.len(),
            1,
            "parse_lines must not split on in-hunk ---; {from_lines:?}"
        );
        assert_eq!(
            (from_lines[0].added(), from_lines[0].removed()),
            (2, 2),
            "parse_lines must keep the same header-looking line counts; hunks={:?}",
            from_lines[0].hunks
        );
    }

    #[test]
    fn test_to_unified_truncation_clears_exact_counts_when_added_lines_are_cut() {
        let filler = "A".repeat(340);
        let old: String = (0..500).map(|i| format!("{filler}{i}\n")).collect();
        let new: String = (0..500).map(|i| format!("B{filler}{i}\n")).collect();
        let diff = FileDiff::from_texts("wide.txt", &old, &new);
        assert_eq!(
            (diff.added(), diff.removed()),
            (500, 500),
            "fixture must retain semantic +500 -500 so a silent render bound is distinguishable \
             from construction elision; elided={:?}",
            diff.elided
        );

        let unified = diff.to_unified();
        let added_rendered = unified
            .lines()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++ "))
            .count();
        let cut = unified.contains("diff rendering truncated");
        assert!(
            cut,
            "fixture must trip MAX_RENDER_CHARS; chars={} added_rendered={} of {}",
            unified.chars().count(),
            added_rendered,
            diff.added()
        );
        assert!(
            added_rendered < diff.added(),
            "truncated unified rendering lost added lines: showed {added_rendered} of {}",
            diff.added()
        );
        assert!(
            !diff.counts_are_exact(),
            "to_unified truncated the rendering (showed {added_rendered} of {} added lines, \
             cut={cut}) but still reports exact counts; elided={:?}",
            diff.added(),
            diff.elided
        );
        assert!(
            !diff.is_complete(),
            "is_complete must be the honest answer after MAX_RENDER_CHARS cuts added lines; \
             file_count_exact={} elided={:?}",
            diff.file_count_is_exact(),
            diff.elided
        );
    }

    #[test]
    fn test_from_texts_keeps_line_truncation_note_when_line_endings_also_change() {
        let old = format!("{}\r\n", "old".repeat(200));
        let new = format!("{}\n", "new".repeat(200));
        let diff = FileDiff::from_texts("mixed.txt", &old, &new);
        let elided = diff.elided.as_deref().unwrap_or("");
        assert!(
            elided.contains("truncated"),
            "long-line truncation must remain visible when line endings also change; a from_texts \
             overwrite of elided would lose that note. elided={elided:?}"
        );
        assert!(
            elided.contains("line endings"),
            "line-ending note must share elided with the truncation note; elided={elided:?}"
        );
        assert!(
            !diff.is_complete(),
            "merged truncation plus line-ending notes must not claim completeness; elided={elided:?}"
        );
    }

    #[test]
    fn test_line_ending_note_alone_does_not_fail_completeness() {
        let diff = FileDiff::from_texts("x", "a\r\n", "a\n");
        assert!(
            diff.elided
                .as_deref()
                .is_some_and(|note| note.contains("line endings")),
            "CRLF to LF must be noted; elided={:?}",
            diff.elided
        );
        assert!(
            diff.counts_are_exact() && diff.is_complete(),
            "an informational line-ending note is not content omission; elided={:?}",
            diff.elided
        );
    }
}
