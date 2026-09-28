use super::identity::ResolvedSource;
use super::identity::MAX_SOURCE_BYTES;
use super::{SourceIdentity, SourceResolver};
use anyhow::{bail, Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use tree_sitter::Language;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

pub(super) const MAX_OUTLINE_RECORDS: usize = 200;
pub(super) const MAX_LABEL_BYTES: usize = 256;
const FALLBACK_WINDOW_LINES: usize = 80;
const MAX_BASE_REF_BYTES: usize = 256;
const MAX_GIT_ERROR_BYTES: usize = 512;

/// A source range using zero-based, half-open UTF-8 byte offsets and
/// one-based, inclusive line numbers. Newline bytes belong to the line they
/// terminate; CRLF is preserved as two source bytes and one line break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

/// Broad evidence class shared by code and document retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetrievalProvenanceClass {
    #[serde(rename = "structural/parser")]
    StructuralParser,
    #[serde(rename = "structural/fallback")]
    StructuralFallback,
    #[serde(rename = "lexical/grep")]
    LexicalGrep,
    #[serde(rename = "semantic/embedding")]
    SemanticEmbedding,
    #[serde(rename = "linguistic/nlp")]
    LinguisticNlp,
    #[serde(rename = "inferred/model")]
    InferredModel,
    #[serde(rename = "confirmed/human")]
    ConfirmedHuman,
}

/// Concrete derivation method within a broad provenance class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMethod {
    TreeSitter,
    MarkdownHeadings,
    FixedWindows,
    Grep,
    Embedding,
    Nlp,
    Model,
    Human,
}

/// Evidence class and concrete backend for a retrieval result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalProvenance {
    pub class: RetrievalProvenanceClass,
    pub method: RetrievalMethod,
}

/// One named structural item. It contains no source body or documentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineRecord {
    pub name: String,
    pub kind: String,
    pub span: SourceSpan,
}

/// Bounded outline tied to exact source bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineResult {
    pub source: SourceIdentity,
    pub provenance: RetrievalProvenance,
    pub records: Vec<OutlineRecord>,
    pub truncated: bool,
    pub parse_had_errors: bool,
}

/// Exact generation-bound source bytes returned from a recorded span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExcerpt {
    pub source: SourceIdentity,
    pub span: SourceSpan,
    pub text: String,
}

/// How one definition's text splits into its signature and body regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureSplit {
    /// Everything before the first opening brace is signature text.
    FirstBrace,
    /// Signature text ends at the line that closes the header with ':'.
    ColonLine,
    /// No body region exists; the whole record text is signature text.
    WholeText,
}

/// Change classification for one symbol between two structural outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineChangeKind {
    Added,
    Removed,
    SignatureChanged,
    BodyChanged,
}

/// One bounded symbol-level delta entry. It carries no source body text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineDiffChange {
    pub name: String,
    pub kind: String,
    pub change: OutlineChangeKind,
    /// Span of the symbol in the working-tree version; absent for removals.
    pub span: Option<SourceSpan>,
}

/// Compact structural delta between a file's working-tree outline and its
/// outline at a Git revision. Bounded and body-free like every outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineDiff {
    pub path: String,
    pub base_ref: String,
    pub source: SourceIdentity,
    pub provenance: RetrievalProvenance,
    pub changes: Vec<OutlineDiffChange>,
    pub base_parse_had_errors: bool,
    pub parse_had_errors: bool,
    pub truncated: bool,
}

impl SourceResolver {
    /// Produce a deterministic, bounded outline for one workspace file.
    pub fn outline(&self, requested: impl AsRef<Path>) -> Result<OutlineResult> {
        let source = self.read(requested)?;
        outline_source(source)
    }

    /// Produce a compact structural delta between one workspace file's
    /// working-tree outline and its outline at a Git revision (`base_ref`).
    /// Symbols are matched by name and kind; a symbol present in both with
    /// identical source bytes is omitted. A symbol whose signature region
    /// (everything before its first brace, or before a header-ending colon
    /// for colon-bodied languages such as Python) changed is reported as
    /// `SignatureChanged`; otherwise a byte-level change is `BodyChanged`.
    /// No source body text is ever included in the result.
    pub fn outline_diff(&self, requested: impl AsRef<Path>, base_ref: &str) -> Result<OutlineDiff> {
        validate_base_ref(base_ref)?;
        let requested = requested.as_ref();
        let current = self.read(requested).with_context(|| {
            format!(
                "code_outline diff mode requires the current file to exist: {}",
                requested.display()
            )
        })?;
        let extension = current
            .canonical
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let Some(language) = LanguageSpec::for_extension(&extension) else {
            bail!(
                "code_outline diff mode does not support the \"{}\" file type: {}",
                extension,
                current.identity.path
            );
        };

        let base_text = fetch_git_blob(self.workspace_root(), base_ref, &current.identity.path)?;
        let (base_records, base_truncated, base_parse_had_errors) = match &base_text {
            Some(text) => tree_sitter_records(text, &language)?,
            None => (Vec::new(), false, false),
        };
        let (current_records, current_truncated, parse_had_errors) =
            tree_sitter_records(&current.text, &language)?;

        let changes = diff_records(
            base_text.as_deref().unwrap_or(""),
            &base_records,
            &current.text,
            &current_records,
        );

        Ok(OutlineDiff {
            path: current.identity.path.clone(),
            base_ref: base_ref.to_string(),
            source: current.identity,
            provenance: RetrievalProvenance {
                class: RetrievalProvenanceClass::StructuralParser,
                method: RetrievalMethod::TreeSitter,
            },
            changes,
            base_parse_had_errors,
            parse_had_errors,
            truncated: base_truncated || current_truncated,
        })
    }

    /// Consume a recorded span only if the same source generation is still
    /// present. Identity verification and slicing use the same bytes read from
    /// one capability-opened file object.
    pub fn read_span(
        &self,
        expected_source: &SourceIdentity,
        span: &SourceSpan,
    ) -> Result<SourceExcerpt> {
        let source = self.read(&expected_source.path)?;
        if source.identity != *expected_source {
            bail!("source generation is stale: {}", expected_source.path);
        }
        if span.start_byte > span.end_byte
            || span.end_byte > source.text.len()
            || !source.text.is_char_boundary(span.start_byte)
            || !source.text.is_char_boundary(span.end_byte)
        {
            bail!("source span is outside UTF-8 boundaries");
        }
        let (actual_start_line, actual_end_line) =
            span_line_coordinates(&source.text, span.start_byte, span.end_byte);
        if span.start_line != actual_start_line || span.end_line != actual_end_line {
            bail!("source span line coordinates do not match its byte coordinates");
        }
        Ok(SourceExcerpt {
            source: source.identity,
            span: span.clone(),
            text: source.text[span.start_byte..span.end_byte].to_string(),
        })
    }
}

pub(super) fn outline_source(source: ResolvedSource) -> Result<OutlineResult> {
    let extension = source
        .canonical
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(extension.as_str(), "md" | "markdown") {
        return Ok(markdown_outline(source));
    }
    if let Some(language) = LanguageSpec::for_extension(&extension) {
        return tree_sitter_outline(source, language);
    }
    Ok(fallback_outline(source))
}

struct LanguageSpec {
    language: Language,
    tags_query: String,
    locals_query: String,
}

impl LanguageSpec {
    fn for_extension(extension: &str) -> Option<Self> {
        let (language, tags_query, locals_query) = match extension {
            "rs" => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::TAGS_QUERY.to_string(),
                String::new(),
            ),
            "go" => (
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::TAGS_QUERY.to_string(),
                String::new(),
            ),
            "ts" => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                ),
                tree_sitter_typescript::LOCALS_QUERY.to_string(),
            ),
            "tsx" => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                ),
                tree_sitter_typescript::LOCALS_QUERY.to_string(),
            ),
            "js" | "jsx" | "mjs" | "cjs" => (
                tree_sitter_javascript::LANGUAGE.into(),
                tree_sitter_javascript::TAGS_QUERY.to_string(),
                tree_sitter_javascript::LOCALS_QUERY.to_string(),
            ),
            "py" => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::TAGS_QUERY.to_string(),
                String::new(),
            ),
            "c" | "h" => (
                tree_sitter_c::LANGUAGE.into(),
                tree_sitter_c::TAGS_QUERY.to_string(),
                String::new(),
            ),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => (
                tree_sitter_cpp::LANGUAGE.into(),
                tree_sitter_cpp::TAGS_QUERY.to_string(),
                String::new(),
            ),
            _ => return None,
        };
        Some(Self {
            language,
            tags_query,
            locals_query,
        })
    }
}

fn tree_sitter_outline(source: ResolvedSource, spec: LanguageSpec) -> Result<OutlineResult> {
    let (records, truncated, parse_had_errors) = tree_sitter_records(&source.text, &spec)?;
    Ok(OutlineResult {
        source: source.identity,
        provenance: RetrievalProvenance {
            class: RetrievalProvenanceClass::StructuralParser,
            method: RetrievalMethod::TreeSitter,
        },
        records,
        truncated,
        parse_had_errors,
    })
}

/// Extract bounded, sorted structural records from raw source text without
/// tying them to a `SourceIdentity`, so both a working-tree read and a Git
/// blob can share the same tag-extraction path for diffing.
fn tree_sitter_records(
    text: &str,
    spec: &LanguageSpec,
) -> Result<(Vec<OutlineRecord>, bool, bool)> {
    let config =
        TagsConfiguration::new(spec.language.clone(), &spec.tags_query, &spec.locals_query)
            .context("failed to configure source tag parser")?;
    let mut context = TagsContext::new();
    let (tags, parse_had_errors) = context
        .generate_tags(&config, text.as_bytes(), None)
        .context("failed to parse source tags")?;
    let mut records = Vec::new();
    let mut truncated = false;
    for tag in tags {
        let tag = tag.context("failed while extracting a source tag")?;
        if !tag.is_definition {
            continue;
        }
        if records.len() == MAX_OUTLINE_RECORDS {
            truncated = true;
            break;
        }
        let name = text[tag.name_range.clone()].trim();
        if name.is_empty() {
            continue;
        }
        let (name, label_truncated) = bounded_label(name);
        truncated |= label_truncated;
        let (start_line, end_line) = span_line_coordinates(text, tag.range.start, tag.range.end);
        records.push(OutlineRecord {
            name,
            kind: config.syntax_type_name(tag.syntax_type_id).to_string(),
            span: SourceSpan {
                start_byte: tag.range.start,
                end_byte: tag.range.end,
                start_line,
                end_line,
            },
        });
    }
    records.sort_by(|left, right| {
        left.span
            .start_byte
            .cmp(&right.span.start_byte)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    records.dedup();
    Ok((records, truncated, parse_had_errors))
}

/// Reject a `base_ref` value before it reaches a `git` argument vector.
fn validate_base_ref(base_ref: &str) -> Result<()> {
    if base_ref.is_empty() {
        bail!("base_ref must not be empty");
    }
    if base_ref.len() > MAX_BASE_REF_BYTES {
        bail!("base_ref exceeds {MAX_BASE_REF_BYTES} bytes");
    }
    if base_ref.starts_with('-') {
        bail!("base_ref must not begin with '-'");
    }
    if base_ref.bytes().any(|byte| byte == 0 || byte == b'\n') {
        bail!("base_ref must not contain control characters");
    }
    Ok(())
}

/// Read one file's UTF-8 text as it existed at `base_ref`. Returns `Ok(None)`
/// when `base_ref` resolves but the path did not exist there (a new file),
/// and a hard error when `base_ref` itself does not resolve to a commit.
fn fetch_git_blob(workspace_root: &Path, base_ref: &str, path: &str) -> Result<Option<String>> {
    let verify = Command::new("git")
        .arg("rev-parse")
        .arg("--verify")
        .arg("--end-of-options")
        .arg(format!("{base_ref}^{{commit}}"))
        .current_dir(workspace_root)
        .output()
        .context("failed to invoke git to verify base_ref")?;
    if !verify.status.success() {
        bail!(
            "base_ref \"{base_ref}\" does not resolve to a commit: {}",
            bounded_git_error(&verify.stderr)
        );
    }

    let show = Command::new("git")
        .arg("show")
        .arg(format!("{base_ref}:{path}"))
        .current_dir(workspace_root)
        .output()
        .context("failed to invoke git to read the base revision")?;
    if !show.status.success() {
        // The path most likely did not exist at base_ref; treat it as new.
        return Ok(None);
    }
    if show.stdout.len() as u64 > MAX_SOURCE_BYTES {
        bail!(
            "base revision of {path} is {} bytes; code_outline diff limit is {MAX_SOURCE_BYTES} bytes",
            show.stdout.len()
        );
    }
    let text = String::from_utf8(show.stdout)
        .map_err(|_| anyhow::anyhow!("base revision of {path} is not UTF-8"))?;
    Ok(Some(text))
}

fn bounded_git_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.len() <= MAX_GIT_ERROR_BYTES {
        return text.to_string();
    }
    let mut end = MAX_GIT_ERROR_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Match base and current records by `(name, kind)` and classify each delta.
/// Matched records with byte-identical spans are omitted entirely.
fn diff_records(
    base_text: &str,
    base_records: &[OutlineRecord],
    current_text: &str,
    current_records: &[OutlineRecord],
) -> Vec<OutlineDiffChange> {
    let mut base_by_key: BTreeMap<(&str, &str), Vec<&OutlineRecord>> = BTreeMap::new();
    for record in base_records {
        base_by_key
            .entry((record.name.as_str(), record.kind.as_str()))
            .or_default()
            .push(record);
    }

    let mut changes = Vec::new();
    for record in current_records {
        let key = (record.name.as_str(), record.kind.as_str());
        let matched = base_by_key
            .get_mut(&key)
            .filter(|slots| !slots.is_empty())
            .map(|slots| slots.remove(0));
        let Some(base_record) = matched else {
            changes.push(OutlineDiffChange {
                name: record.name.clone(),
                kind: record.kind.clone(),
                change: OutlineChangeKind::Added,
                span: Some(record.span.clone()),
            });
            continue;
        };
        let base_span_text = &base_text[base_record.span.start_byte..base_record.span.end_byte];
        let current_span_text = &current_text[record.span.start_byte..record.span.end_byte];
        if base_span_text == current_span_text {
            continue;
        }
        let (_, base_signature, _) = split_signature_body(base_span_text);
        let (_, current_signature, _) = split_signature_body(current_span_text);
        let change = if base_signature.trim() != current_signature.trim() {
            OutlineChangeKind::SignatureChanged
        } else {
            OutlineChangeKind::BodyChanged
        };
        changes.push(OutlineDiffChange {
            name: record.name.clone(),
            kind: record.kind.clone(),
            change,
            span: Some(record.span.clone()),
        });
    }

    let mut removed: Vec<&OutlineRecord> = base_by_key.into_values().flatten().collect();
    removed.sort_by_key(|record| record.span.start_byte);
    for base_record in removed {
        changes.push(OutlineDiffChange {
            name: base_record.name.clone(),
            kind: base_record.kind.clone(),
            change: OutlineChangeKind::Removed,
            span: None,
        });
    }
    changes
}

/// Split one definition's source text into a signature region (compared to
/// decide `SignatureChanged`) and a body region. Braces mark the boundary
/// where a language has them; otherwise a header-ending colon (Python-style
/// blocks) does; otherwise the whole text is signature with no body.
fn split_signature_body(text: &str) -> (SignatureSplit, &str, &str) {
    if let Some(brace_index) = text.find('{') {
        return (
            SignatureSplit::FirstBrace,
            &text[..brace_index],
            &text[brace_index..],
        );
    }
    if let Some(split_at) = colon_line_end(text) {
        return (
            SignatureSplit::ColonLine,
            &text[..split_at],
            &text[split_at..],
        );
    }
    (SignatureSplit::WholeText, text, "")
}

/// Byte offset just after the colon on the first line (scanning from the
/// start of `text`) whose trailing whitespace-trimmed content ends with
/// `:`. Returns `None` when no such line exists.
fn colon_line_end(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let core = line.trim_end_matches(['\n', '\r']).trim_end();
        if core.ends_with(':') {
            return Some(offset + core.len());
        }
        offset += line.len();
    }
    None
}

fn markdown_outline(source: ResolvedSource) -> OutlineResult {
    let mut records = Vec::new();
    let mut truncated = false;
    let mut heading: Option<(HeadingLevel, usize, String)> = None;
    for (event, range) in Parser::new(&source.text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level, range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if heading.is_some() => {
                heading.as_mut().expect("checked heading").2.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if heading.is_some() => {
                heading.as_mut().expect("checked heading").2.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((level, start_byte, label)) = heading.take() else {
                    continue;
                };
                if records.len() == MAX_OUTLINE_RECORDS {
                    truncated = true;
                    break;
                }
                let (name, label_truncated) = bounded_label(label.trim());
                truncated |= label_truncated;
                if name.is_empty() {
                    continue;
                }
                let (start_line, end_line) =
                    span_line_coordinates(&source.text, start_byte, range.end);
                records.push(OutlineRecord {
                    name,
                    kind: format!("heading_{}", heading_level_number(level)),
                    span: SourceSpan {
                        start_byte,
                        end_byte: range.end,
                        start_line,
                        end_line,
                    },
                });
            }
            _ => {}
        }
    }
    OutlineResult {
        source: source.identity,
        provenance: RetrievalProvenance {
            class: RetrievalProvenanceClass::StructuralParser,
            method: RetrievalMethod::MarkdownHeadings,
        },
        records,
        truncated,
        parse_had_errors: false,
    }
}

fn fallback_outline(source: ResolvedSource) -> OutlineResult {
    let lines = lines_with_offsets(&source.text);
    let mut records = Vec::new();
    let mut truncated = false;
    for (window_index, chunk) in lines.chunks(FALLBACK_WINDOW_LINES).enumerate() {
        if records.len() == MAX_OUTLINE_RECORDS {
            truncated = true;
            break;
        }
        let Some(first) = chunk.first() else {
            break;
        };
        let last = chunk.last().expect("non-empty line chunk");
        let start_line = window_index * FALLBACK_WINDOW_LINES + 1;
        let end_line = start_line + chunk.len() - 1;
        records.push(OutlineRecord {
            name: format!("lines {start_line}-{end_line}"),
            kind: "window".to_string(),
            span: SourceSpan {
                start_byte: first.start,
                end_byte: last.end,
                start_line,
                end_line,
            },
        });
    }
    OutlineResult {
        source: source.identity,
        provenance: RetrievalProvenance {
            class: RetrievalProvenanceClass::StructuralFallback,
            method: RetrievalMethod::FixedWindows,
        },
        records,
        truncated,
        parse_had_errors: false,
    }
}

fn bounded_label(label: &str) -> (String, bool) {
    if label.len() <= MAX_LABEL_BYTES {
        return (label.to_string(), false);
    }
    let mut end = MAX_LABEL_BYTES;
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    (label[..end].to_string(), true)
}

pub(super) fn span_line_coordinates(
    text: &str,
    start_byte: usize,
    end_byte: usize,
) -> (usize, usize) {
    let start_line = text.as_bytes()[..start_byte]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1;
    let end_line = if end_byte == start_byte {
        start_line
    } else {
        text.as_bytes()[..end_byte - 1]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1
    };
    (start_line, end_line)
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

struct Line {
    start: usize,
    end: usize,
}

fn lines_with_offsets(text: &str) -> Vec<Line> {
    let mut offset = 0;
    text.split_inclusive('\n')
        .map(|text| {
            let start = offset;
            offset += text.len();
            Line { start, end: offset }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn outline(extension: &str, source: &str) -> OutlineResult {
        let workspace = tempfile::tempdir().expect("workspace");
        let file = workspace.path().join(format!("fixture.{extension}"));
        fs::write(&file, source).expect("fixture source");
        SourceResolver::new(workspace.path())
            .expect("source resolver")
            .outline(&file)
            .expect("source outline")
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A tempdir Git repo with one file committed at HEAD as the "base"
    /// version. Callers then overwrite the file in place to produce the
    /// "current" working-tree version `outline_diff` compares against it.
    fn git_workspace_with_base_commit(
        filename: &str,
        base_source: &str,
    ) -> (tempfile::TempDir, SourceResolver) {
        let workspace = tempfile::tempdir().expect("workspace");
        run_git(workspace.path(), &["init", "-q"]);
        fs::write(workspace.path().join(filename), base_source).expect("base source");
        run_git(workspace.path(), &["add", filename]);
        run_git(
            workspace.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "base",
            ],
        );
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        (workspace, resolver)
    }

    #[test]
    fn test_tree_sitter_outlines_supported_languages_without_body_text() {
        let fixtures = [
            (
                "rs",
                "const SECRET: &str = \"never emit me\";\nfn alpha() {}\nstruct Beta;\n",
                "alpha",
            ),
            (
                "go",
                "package fixture\nfunc Alpha() {}\ntype Beta struct{}\n",
                "Alpha",
            ),
            (
                "ts",
                "export function alpha() {}\nexport class Beta {}\n",
                "alpha",
            ),
            (
                "js",
                "export function alpha() {}\nexport class Beta {}\n",
                "alpha",
            ),
            (
                "py",
                "def alpha():\n    pass\nclass Beta:\n    pass\n",
                "alpha",
            ),
            (
                "c",
                "int alpha(void) { return 0; }\nstruct Beta { int n; };\n",
                "alpha",
            ),
            (
                "cpp",
                "int alpha() { return 0; }\nclass Beta {};\n",
                "alpha",
            ),
        ];
        for (extension, source, expected_name) in fixtures {
            let result = outline(extension, source);
            let json = serde_json::to_string(&result).expect("outline JSON");
            assert!(
                result
                    .records
                    .iter()
                    .any(|record| record.name == expected_name),
                "{extension} outline must contain the expected definition: {json}"
            );
            assert!(
                !json.contains("never emit me") && !json.contains("return 0"),
                "{extension} outline must not leak comments, strings, or bodies: {json}"
            );
        }
    }

    #[test]
    fn test_multiline_tree_sitter_span_round_trips_through_read_span() {
        let workspace = tempfile::tempdir().expect("workspace");
        let source = "fn multiline(\n    value: usize,\n) -> usize {\n    value + 1\n}\n";
        fs::write(workspace.path().join("fixture.rs"), source).expect("fixture source");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        let outline = resolver.outline("fixture.rs").expect("outline");
        let record = outline
            .records
            .iter()
            .find(|record| record.name == "multiline")
            .expect("multiline definition");

        let excerpt = resolver
            .read_span(&outline.source, &record.span)
            .expect("parser-produced span must be consumable");
        assert!(excerpt.text.starts_with("fn multiline("));
        assert!(excerpt.text.contains("value + 1"));
        assert_eq!(excerpt.span.start_line, 1);
        assert_eq!(excerpt.span.end_line, 5);
    }

    #[test]
    fn test_markdown_outline_carries_heading_spans_only() {
        let result = outline("md", "# One\nsecret paragraph\n\nTwo\n---\n");
        assert_eq!(
            result.provenance,
            RetrievalProvenance {
                class: RetrievalProvenanceClass::StructuralParser,
                method: RetrievalMethod::MarkdownHeadings,
            }
        );
        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert!(
            !serde_json::to_string(&result)
                .expect("outline JSON")
                .contains("secret paragraph"),
            "paragraph bodies must not enter the structural outline"
        );
    }

    #[test]
    fn test_markdown_outline_ignores_headings_inside_code_blocks() {
        let result = outline(
            "md",
            "# Public\n\n```markdown\n# fenced_secret\n```\n\n    # indented_secret\n\n<!--\n# comment_secret\n-->\n\n- item\n\n  ```markdown\n  # list_secret\n  ```\n\n> ```markdown\n> # quote_secret\n> ```\n\nVisible\n---\n",
        );
        let names = result
            .records
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Public", "Visible"]);
        let json = serde_json::to_string(&result).expect("outline JSON");
        assert!(!json.contains("fenced_secret"), "{json}");
        assert!(!json.contains("indented_secret"), "{json}");
        assert!(!json.contains("comment_secret"), "{json}");
        assert!(!json.contains("list_secret"), "{json}");
        assert!(!json.contains("quote_secret"), "{json}");
    }

    #[test]
    fn test_unknown_text_uses_bounded_body_free_windows() {
        let source = (1..=161)
            .map(|line| format!("private payload {line}\n"))
            .collect::<String>();
        let result = outline("txt", &source);
        assert_eq!(
            result.provenance,
            RetrievalProvenance {
                class: RetrievalProvenanceClass::StructuralFallback,
                method: RetrievalMethod::FixedWindows,
            }
        );
        assert_eq!(result.records.len(), 3);
        let json = serde_json::to_string(&result).expect("outline JSON");
        assert!(json.contains("lines 1-80") && json.contains("lines 161-161"));
        assert!(
            !json.contains("private payload"),
            "fallback labels must not copy source bodies: {json}"
        );
    }

    #[test]
    fn test_outline_record_count_is_bounded_and_reports_truncation() {
        let source = (0..=MAX_OUTLINE_RECORDS)
            .map(|index| format!("fn item_{index}() {{}}\n"))
            .collect::<String>();
        let result = outline("rs", &source);
        assert_eq!(result.records.len(), MAX_OUTLINE_RECORDS);
        assert!(result.truncated);
    }

    #[test]
    fn test_outline_labels_are_bounded_without_splitting_utf8() {
        let long_name = format!("{}tail", "界".repeat(MAX_LABEL_BYTES));
        let result = outline("md", &format!("# {long_name}\n"));
        assert!(result.truncated);
        assert!(result.records[0].name.len() <= MAX_LABEL_BYTES);
        assert!(result.records[0]
            .name
            .is_char_boundary(result.records[0].name.len()));
    }

    #[test]
    fn test_read_span_round_trips_utf8_and_crlf_coordinates() {
        let workspace = tempfile::tempdir().expect("workspace");
        let source = "# α\r\nbody\r\n";
        fs::write(workspace.path().join("fixture.md"), source).expect("fixture source");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        let outline = resolver.outline("fixture.md").expect("outline");
        let heading = &outline.records[0];
        assert_eq!(heading.span.start_byte, 0);
        assert_eq!(heading.span.end_byte, "# α\r\n".len());
        assert_eq!(heading.span.start_line, 1);
        assert_eq!(heading.span.end_line, 1);

        let excerpt = resolver
            .read_span(&outline.source, &heading.span)
            .expect("generation-bound span");
        assert_eq!(excerpt.text, "# α\r\n");
    }

    #[test]
    fn test_read_span_rejects_stale_generation_without_new_bytes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("fixture.md");
        fs::write(&path, "# old\n").expect("old source");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        let outline = resolver.outline("fixture.md").expect("outline");
        fs::write(&path, "# new secret\n").expect("new source");

        let error = resolver
            .read_span(&outline.source, &outline.records[0].span)
            .expect_err("stale span must fail closed");
        assert!(error.to_string().contains("stale"), "{error:#}");
        assert!(!error.to_string().contains("new secret"));
    }

    #[test]
    fn test_provenance_serialization_is_shared_class_plus_backend() {
        let result = outline("rs", "fn alpha() {}\n");
        let json = serde_json::to_value(result.provenance).expect("provenance JSON");
        assert_eq!(
            json,
            serde_json::json!({
                "class": "structural/parser",
                "method": "tree_sitter"
            })
        );
    }

    #[test]
    fn test_validate_base_ref_rejects_empty_oversized_and_dash_prefixed_values() {
        assert!(
            validate_base_ref("").is_err(),
            "empty base_ref must be rejected"
        );
        assert!(
            validate_base_ref(&"a".repeat(MAX_BASE_REF_BYTES + 1)).is_err(),
            "an over-long base_ref must be rejected"
        );
        assert!(
            validate_base_ref("-oops").is_err(),
            "a base_ref starting with '-' must be rejected before it can be read as a git flag"
        );
        assert!(validate_base_ref("HEAD~1").is_ok());
    }

    #[test]
    fn test_outline_diff_reports_added_symbol() {
        let (workspace, resolver) = git_workspace_with_base_commit("sample.rs", "fn kept() {}\n");
        fs::write(
            workspace.path().join("sample.rs"),
            "fn kept() {}\nfn added() {}\n",
        )
        .expect("current source");

        let diff = resolver
            .outline_diff("sample.rs", "HEAD")
            .expect("outline diff");
        assert_eq!(
            diff.changes
                .iter()
                .map(|change| (change.name.as_str(), change.change))
                .collect::<Vec<_>>(),
            vec![("added", OutlineChangeKind::Added)],
            "only the newly added symbol should appear in the delta: {diff:?}"
        );
        assert!(
            diff.changes[0].span.is_some(),
            "an added symbol must carry its current span"
        );
    }

    #[test]
    fn test_outline_diff_reports_removed_symbol() {
        let (workspace, resolver) =
            git_workspace_with_base_commit("sample.rs", "fn kept() {}\nfn gone() {}\n");
        fs::write(workspace.path().join("sample.rs"), "fn kept() {}\n").expect("current source");

        let diff = resolver
            .outline_diff("sample.rs", "HEAD")
            .expect("outline diff");
        assert_eq!(
            diff.changes
                .iter()
                .map(|change| (change.name.as_str(), change.change, change.span.is_none()))
                .collect::<Vec<_>>(),
            vec![("gone", OutlineChangeKind::Removed, true)],
            "a removed symbol must be reported with no current span: {diff:?}"
        );
    }

    #[test]
    fn test_outline_diff_reports_signature_changed() {
        let (workspace, resolver) =
            git_workspace_with_base_commit("sample.rs", "fn edited(a: usize) {}\n");
        fs::write(workspace.path().join("sample.rs"), "fn edited(a: u64) {}\n")
            .expect("current source");

        let diff = resolver
            .outline_diff("sample.rs", "HEAD")
            .expect("outline diff");
        assert_eq!(diff.changes.len(), 1, "{diff:?}");
        assert_eq!(diff.changes[0].name, "edited");
        assert_eq!(
            diff.changes[0].change,
            OutlineChangeKind::SignatureChanged,
            "a parameter-type edit must be classified as a signature change: {diff:?}"
        );
    }

    #[test]
    fn test_outline_diff_reports_body_changed_when_signature_unchanged() {
        let (workspace, resolver) =
            git_workspace_with_base_commit("sample.rs", "fn edited(a: usize) { a + 1; }\n");
        fs::write(
            workspace.path().join("sample.rs"),
            "fn edited(a: usize) { a + 2; }\n",
        )
        .expect("current source");

        let diff = resolver
            .outline_diff("sample.rs", "HEAD")
            .expect("outline diff");
        assert_eq!(diff.changes.len(), 1, "{diff:?}");
        assert_eq!(diff.changes[0].name, "edited");
        assert_eq!(
            diff.changes[0].change,
            OutlineChangeKind::BodyChanged,
            "an unchanged signature with a changed body must not be reported as a signature change: {diff:?}"
        );
        let json = serde_json::to_string(&diff).expect("diff JSON");
        assert!(
            !json.contains("a + 2"),
            "diff mode must not leak body text: {json}"
        );
    }

    #[test]
    fn test_outline_diff_omits_byte_identical_symbol() {
        let source = "fn stable() { 1 }\n";
        let (workspace, resolver) = git_workspace_with_base_commit("sample.rs", source);
        fs::write(workspace.path().join("sample.rs"), source).expect("current source == base");

        let diff = resolver
            .outline_diff("sample.rs", "HEAD")
            .expect("outline diff");
        assert!(
            diff.changes.is_empty(),
            "a byte-identical symbol must not appear in the delta: {diff:?}"
        );
    }

    #[test]
    fn test_outline_diff_detects_signature_and_body_changes_in_python() {
        let base = "def greet(name):\n    return 'hi ' + name\n\ndef stable():\n    return 1\n";
        let (workspace, resolver) = git_workspace_with_base_commit("sample.py", base);
        let current =
            "def greet(name, loud=False):\n    return 'hi ' + name\n\ndef stable():\n    return 2\n";
        fs::write(workspace.path().join("sample.py"), current).expect("current source");

        let diff = resolver
            .outline_diff("sample.py", "HEAD")
            .expect("outline diff");
        let change_of = |name: &str| {
            diff.changes
                .iter()
                .find(|change| change.name == name)
                .map(|change| change.change)
        };
        assert_eq!(
            change_of("greet"),
            Some(OutlineChangeKind::SignatureChanged),
            "an added parameter is a signature change for a colon-headed definition too: {diff:?}"
        );
        assert_eq!(
            change_of("stable"),
            Some(OutlineChangeKind::BodyChanged),
            "{diff:?}"
        );
    }

    #[test]
    fn test_outline_diff_treats_missing_base_path_as_a_new_file() {
        let (workspace, resolver) =
            git_workspace_with_base_commit("other.rs", "fn unrelated() {}\n");
        fs::write(workspace.path().join("fresh.rs"), "fn brand_new() {}\n")
            .expect("new file, never committed");

        let diff = resolver
            .outline_diff("fresh.rs", "HEAD")
            .expect("a base_ref that predates the file must diff cleanly, not error");
        assert_eq!(diff.changes.len(), 1, "{diff:?}");
        assert_eq!(diff.changes[0].name, "brand_new");
        assert_eq!(
            diff.changes[0].change,
            OutlineChangeKind::Added,
            "every symbol in a file absent from base_ref must show as added: {diff:?}"
        );
    }

    #[test]
    fn test_outline_diff_rejects_unresolvable_base_ref() {
        let (workspace, resolver) = git_workspace_with_base_commit("sample.rs", "fn kept() {}\n");
        let _keep_alive = &workspace;

        let error = resolver
            .outline_diff("sample.rs", "not-a-real-ref")
            .expect_err(
                "an unresolvable base_ref must fail closed, not silently diff against nothing",
            );
        assert!(
            error.to_string().contains("not-a-real-ref"),
            "error must name the offending base_ref: {error:#}"
        );
    }

    #[test]
    fn test_outline_diff_fails_closed_when_working_tree_file_is_missing() {
        let (workspace, resolver) = git_workspace_with_base_commit("sample.rs", "fn kept() {}\n");
        fs::remove_file(workspace.path().join("sample.rs")).expect("delete working-tree file");

        let error = resolver.outline_diff("sample.rs", "HEAD").expect_err(
            "diff mode must fail closed, not panic, when the current file no longer exists",
        );
        assert!(
            error.to_string().contains("sample.rs"),
            "error must name the missing file: {error:#}"
        );
    }

    #[test]
    fn test_outline_diff_rejects_unsupported_language() {
        let (workspace, resolver) = git_workspace_with_base_commit("notes.txt", "first\n");
        fs::write(workspace.path().join("notes.txt"), "second\n").expect("current source");

        let error = resolver
            .outline_diff("notes.txt", "HEAD")
            .expect_err("fallback/plain-text outlines must not silently produce an empty diff");
        assert!(
            error.to_string().contains("txt"),
            "error must name the unsupported file type: {error:#}"
        );
    }
}
