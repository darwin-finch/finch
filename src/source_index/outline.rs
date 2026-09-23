use super::identity::ResolvedSource;
use super::{SourceIdentity, SourceResolver};
use anyhow::{bail, Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tree_sitter::Language;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

pub(super) const MAX_OUTLINE_RECORDS: usize = 200;
pub(super) const MAX_LABEL_BYTES: usize = 256;
const FALLBACK_WINDOW_LINES: usize = 80;

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

impl SourceResolver {
    /// Produce a deterministic, bounded outline for one workspace file.
    pub fn outline(&self, requested: impl AsRef<Path>) -> Result<OutlineResult> {
        let source = self.read(requested)?;
        outline_source(source)
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
    let config = TagsConfiguration::new(spec.language, &spec.tags_query, &spec.locals_query)
        .context("failed to configure source tag parser")?;
    let mut context = TagsContext::new();
    let (tags, parse_had_errors) = context
        .generate_tags(&config, source.text.as_bytes(), None)
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
        let name = source.text[tag.name_range.clone()].trim();
        if name.is_empty() {
            continue;
        }
        let (name, label_truncated) = bounded_label(name);
        truncated |= label_truncated;
        let (start_line, end_line) =
            span_line_coordinates(&source.text, tag.range.start, tag.range.end);
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
}
