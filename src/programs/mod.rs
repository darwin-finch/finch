//! Persistent, language-neutral program vocabulary.
//!
//! Forth and Lisp lower into Finch's shared typed runtime. This module gives
//! definitions a shared identity, metadata model, and discovery manifest; it
//! must never select a legacy evaluator as an alternate invocation ABI.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Version of the model/runtime vocabulary handshake.
pub const MANIFEST_PROTOCOL_VERSION: u32 = 1;

/// Minimal language/runtime definition supplied to every fresh model context.
pub const BOOT_CAPSULE: &str = include_str!("../../vocabulary/BOOT.md");
pub const VM_LANGUAGE_DEFINITION: &str = include_str!("../../vocabulary/language/FINCH_VM.md");
pub const FORTH_LANGUAGE_DEFINITION: &str =
    include_str!("../../vocabulary/language/FINCH_FORTH.md");
pub const LISP_LANGUAGE_DEFINITION: &str = include_str!("../../vocabulary/language/FINCH_LISP.md");
pub const LANGUAGE_SCHEMA: &str = include_str!("../../vocabulary/language/schema.json");

/// One complete Co-Forth lexical token observed while a provider response is
/// still streaming. The runtime must not execute these incrementally: callers
/// use them only for safe progress/display projection before the complete
/// source is compiled and verified at the program boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForthWireToken {
    pub start_byte: usize,
    pub end_byte: usize,
    pub source: String,
}

/// Incremental lexical receiver for the compact Co-Forth wire form.
///
/// It accepts arbitrary UTF-8 fragments, including fragments that split a
/// word, escape, raw string delimiter, or comment. `push` yields only tokens
/// known to be complete. `finish` turns an incomplete quoted literal into a
/// clear wire error and releases a final unterminated word. This deliberately
/// does not type-check or execute a prefix; full typed verification remains an
/// all-program operation.
#[derive(Debug, Default)]
pub struct ForthWireBuffer {
    source: String,
    emitted_tokens: usize,
}

impl ForthWireBuffer {
    pub fn push(&mut self, fragment: &str) -> Result<Vec<ForthWireToken>> {
        self.source.push_str(fragment);
        self.collect_complete(false)
    }

    pub fn finish(&mut self) -> Result<Vec<ForthWireToken>> {
        self.collect_complete(true)
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    fn collect_complete(&mut self, final_boundary: bool) -> Result<Vec<ForthWireToken>> {
        let ranges = complete_forth_wire_tokens(&self.source, final_boundary)?;
        let new_ranges = ranges
            .get(self.emitted_tokens..)
            .expect("append-only source cannot lose completed wire tokens");
        let tokens = new_ranges
            .iter()
            .map(|&(start_byte, end_byte)| ForthWireToken {
                start_byte,
                end_byte,
                source: self.source[start_byte..end_byte].to_string(),
            })
            .collect::<Vec<_>>();
        self.emitted_tokens = ranges.len();
        Ok(tokens)
    }
}

fn complete_forth_wire_tokens(source: &str, final_boundary: bool) -> Result<Vec<(usize, usize)>> {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();

    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        if bytes[cursor] == b'\\' {
            match source[cursor..].find('\n') {
                Some(offset) => {
                    cursor += offset + 1;
                    continue;
                }
                None if final_boundary => break,
                None => break,
            }
        }

        let start = cursor;
        if source[start..].starts_with("s\"\"\"") {
            let content_start = start + 4;
            match source[content_start..].find("\"\"\"") {
                Some(offset) => {
                    cursor = content_start + offset + 3;
                    tokens.push((start, cursor));
                    continue;
                }
                None if final_boundary => bail!("unterminated Co-Forth raw string literal"),
                None => break,
            }
        }
        // `s"..."` pushes a string and standard Forth `."..."` emits one.
        // Both use the same escaping and optional single-space delimiter, so
        // the wire receiver must keep either form intact while it is streamed.
        if source[start..].starts_with("s\"") || source[start..].starts_with(".\"") {
            cursor += 2;
            if cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let mut escaped = false;
            let mut closed = None;
            while cursor < source.len() {
                let character = source[cursor..]
                    .chars()
                    .next()
                    .expect("cursor remains on a UTF-8 boundary");
                cursor += character.len_utf8();
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    closed = Some(cursor);
                    break;
                }
            }
            match closed {
                Some(end) => {
                    tokens.push((start, end));
                    continue;
                }
                None if final_boundary => bail!("unterminated Co-Forth string literal"),
                None => break,
            }
        }

        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() && !final_boundary {
            break;
        }
        tokens.push((start, cursor));
    }
    Ok(tokens)
}

/// Identity of one normative artifact handed to a provider. The body is
/// retrieved on demand, while the manifest carries this compact fingerprint so
/// a resumed/provider-switched model can tell exactly which contract it saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguagePackageIdentity {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

pub fn language_package_identities() -> Vec<LanguagePackageIdentity> {
    [
        ("boot", "FINCH-VM-TYPED/1", BOOT_CAPSULE),
        ("shared", "FINCH-VM-TYPED/1", VM_LANGUAGE_DEFINITION),
        ("forth", "FINCH-FORTH/1", FORTH_LANGUAGE_DEFINITION),
        ("lisp", "FINCH-LISP/1", LISP_LANGUAGE_DEFINITION),
        ("schema", "FINCH-SCHEMA/1", LANGUAGE_SCHEMA),
    ]
    .into_iter()
    .map(|(name, version, contents)| LanguagePackageIdentity {
        name: name.to_string(),
        version: version.to_string(),
        sha256: hash_text(contents),
    })
    .collect()
}

/// Upper bound on what executing a program may affect.
///
/// This is deliberately about observable effects, not implementation language.
/// Pure and read-only programs can run autonomously; mutation and unknown code
/// cross an approval boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEffect {
    Pure,
    VmRead,
    VmWrite,
    WorkspaceRead,
    ExternalRead,
    WorkspaceWrite,
    ExternalWrite,
    Destructive,
    Unclassified,
}

impl ExecutionEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::VmRead => "vm_read",
            Self::VmWrite => "vm_write",
            Self::WorkspaceRead => "workspace_read",
            Self::ExternalRead => "external_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::ExternalWrite => "external_write",
            Self::Destructive => "destructive",
            Self::Unclassified => "unclassified",
        }
    }

    pub fn runs_autonomously(self) -> bool {
        matches!(
            self,
            Self::Pure | Self::VmRead | Self::VmWrite | Self::WorkspaceRead | Self::ExternalRead
        )
    }
}

impl std::str::FromStr for ExecutionEffect {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pure" => Ok(Self::Pure),
            "vm_read" => Ok(Self::VmRead),
            "vm_write" => Ok(Self::VmWrite),
            "workspace_read" => Ok(Self::WorkspaceRead),
            "external_read" => Ok(Self::ExternalRead),
            "workspace_write" => Ok(Self::WorkspaceWrite),
            "external_write" => Ok(Self::ExternalWrite),
            "destructive" => Ok(Self::Destructive),
            "unclassified" => Ok(Self::Unclassified),
            other => bail!("unknown execution effect: {other}"),
        }
    }
}

/// Language in which a stored program's canonical source is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

impl ProgramLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forth => "forth",
            Self::Lisp => "lisp",
        }
    }

    /// Compact wire-format inference used only when the submission envelope
    /// omits `language`; the resolved value is recorded before execution.
    pub fn infer_source(source: &str) -> Self {
        if source.trim_start().starts_with('(') {
            Self::Lisp
        } else {
            Self::Forth
        }
    }

    /// Resolve the compact provider wire form before parsing. The first
    /// non-whitespace byte remains the intentionally cheap discriminator,
    /// while common non-protocol wrappers receive a useful error instead of
    /// being misreported as an unknown Co-Forth word.
    pub fn infer_wire_source(source: &str) -> Result<Self> {
        let trimmed = source.trim_start();
        if trimmed.is_empty() {
            bail!("E-WIRE-001: Finch wire response is empty; emit a Lisp or Co-Forth program")
        }
        if trimmed.starts_with("```") {
            bail!(
                "E-WIRE-002: Finch wire response must be raw Lisp/Co-Forth, not a Markdown code fence; \
                 emit s\"...\" say for user prose"
            )
        }
        Ok(Self::infer_source(trimmed))
    }
}

impl std::str::FromStr for ProgramLanguage {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "forth" => Ok(Self::Forth),
            "lisp" => Ok(Self::Lisp),
            other => bail!("unknown program language: {other}"),
        }
    }
}

/// A self-executing Finch source file after its shebang has been removed.
///
/// This is deliberately only a parsed envelope. Callers submit `source` to
/// the normal typed `ProgramRuntime`; a shebang can select a language, but it
/// can never grant capabilities or bypass approval policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinchScript {
    pub language: ProgramLanguage,
    pub source: String,
}

/// Parse a Finch executable script header.
///
/// Supported form: `#!/path/to/finch --exec [--language=lisp|forth]`.
/// The operating system passes the script path after the one optional shebang
/// argument, so `--exec` is intentionally the only required execution-mode
/// flag. Language selection is explicit when supplied, otherwise it follows
/// `.lisp`/`.forth` extensions and finally the compact wire inference rule.
pub fn parse_finch_script(path: &Path, contents: &str) -> Result<FinchScript> {
    let Some((header, source)) = contents.split_once('\n') else {
        bail!(
            "Finch script '{}' has no source after its shebang",
            path.display()
        );
    };
    let header = header.strip_suffix('\r').unwrap_or(header);
    let Some(command) = header.strip_prefix("#!") else {
        bail!(
            "Finch script '{}' must start with a Finch shebang",
            path.display()
        );
    };
    let mut parts = command.split_whitespace();
    if parts.next().is_none() {
        bail!("Finch script '{}' has an empty shebang", path.display());
    }

    let mut exec = false;
    let mut language = None;
    while let Some(argument) = parts.next() {
        match argument {
            "--exec" => exec = true,
            "--language" => {
                let Some(value) = parts.next() else {
                    bail!(
                        "Finch script '{}' is missing a language value",
                        path.display()
                    );
                };
                language = Some(value.parse()?);
            }
            value if value.starts_with("--language=") => {
                language = Some(value["--language=".len()..].parse()?);
            }
            // The interpreter portion of a platform shebang is not Finch VM
            // metadata. Ignore it: the operating system or explicit CLI
            // invocation has already selected Finch before this parser runs.
            other if !other.starts_with('-') => continue,
            other => bail!(
                "unsupported Finch script shebang option '{other}' in '{}'",
                path.display()
            ),
        }
    }
    if !exec {
        bail!(
            "Finch script '{}' must use --exec; this does not bypass capability checks",
            path.display()
        );
    }
    let language = match language {
        Some(language) => language,
        None => match path.extension().and_then(|value| value.to_str()) {
            Some("lisp") => ProgramLanguage::Lisp,
            Some("forth") => ProgramLanguage::Forth,
            _ => ProgramLanguage::infer_wire_source(source)?,
        },
    };
    Ok(FinchScript {
        language,
        source: source.to_string(),
    })
}

/// Persistence and visibility boundary for a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramScope {
    Builtin,
    Task,
    Session,
    Project,
    Personal,
    User,
    Published,
    Imported,
}

impl ProgramScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Task => "task",
            Self::Session => "session",
            Self::Project => "project",
            Self::Personal => "personal",
            Self::User => "user",
            Self::Published => "published",
            Self::Imported => "imported",
        }
    }
}

impl std::str::FromStr for ProgramScope {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "task" => Ok(Self::Task),
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "personal" => Ok(Self::Personal),
            "user" => Ok(Self::User),
            "published" => Ok(Self::Published),
            "imported" => Ok(Self::Imported),
            other => bail!("unknown program scope: {other}"),
        }
    }
}

/// Review state for executable vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    Candidate,
    Tested,
    Approved,
    Quarantined,
    Deprecated,
}

impl TrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Tested => "tested",
            Self::Approved => "approved",
            Self::Quarantined => "quarantined",
            Self::Deprecated => "deprecated",
        }
    }
}

impl std::str::FromStr for TrustState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "tested" => Ok(Self::Tested),
            "approved" => Ok(Self::Approved),
            "quarantined" => Ok(Self::Quarantined),
            "deprecated" => Ok(Self::Deprecated),
            other => bail!("unknown program trust state: {other}"),
        }
    }
}

/// Immutable address of a stored program version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProgramRef {
    pub id: Uuid,
    pub version: u64,
}

/// Portable values accepted by the initial pure cross-language ABI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ProgramValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Symbol(String),
    String(String),
    Bytes(Vec<u8>),
    /// A portable, managed JSON tree returned by the typed VM. This preserves
    /// its structured representation across the runtime boundary rather than
    /// degrading it to an untyped string.
    Json(serde_json::Value),
    List(Vec<ProgramValue>),
    Map(Vec<(ProgramValue, ProgramValue)>),
    Option(Option<Box<ProgramValue>>),
    Result {
        ok: bool,
        value: Box<ProgramValue>,
    },
    Task(String),
    Resource {
        kind: String,
        handle: String,
        generation: u64,
    },
}

/// Canonical definition and review metadata for one program version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramDefinition {
    pub reference: ProgramRef,
    pub name: String,
    pub language: ProgramLanguage,
    pub source: String,
    pub documentation: String,
    pub signature: Option<String>,
    pub effect: ExecutionEffect,
    pub capabilities: Vec<String>,
    pub dependencies: Vec<ProgramRef>,
    pub tests: Vec<String>,
    pub provenance: String,
    pub trust: TrustState,
    pub scope: ProgramScope,
    pub scope_key: Option<String>,
    pub source_hash: String,
    pub environment_hash: String,
}

impl ProgramDefinition {
    /// Create a new session candidate. Persistence assigns its final version.
    pub fn candidate(
        name: impl Into<String>,
        language: ProgramLanguage,
        source: impl Into<String>,
    ) -> Self {
        let source = source.into();
        let effect = declared_effect(&source, language).unwrap_or(ExecutionEffect::Unclassified);
        Self {
            reference: ProgramRef {
                id: Uuid::new_v4(),
                version: 0,
            },
            name: name.into(),
            language,
            source_hash: hash_text(&source),
            source,
            documentation: String::new(),
            signature: None,
            effect,
            capabilities: Vec::new(),
            dependencies: Vec::new(),
            tests: Vec::new(),
            provenance: "agent".to_string(),
            trust: TrustState::Candidate,
            scope: ProgramScope::Session,
            scope_key: None,
            environment_hash: "unbound".to_string(),
        }
    }

    /// Project an existing Co-Forth vocabulary entry into the shared registry.
    pub fn from_forth_entry(
        entry: &crate::coforth::WordEntry,
        scope: ProgramScope,
    ) -> Option<Self> {
        let source = entry.forth.clone()?;
        let sense = entry.sense.as_deref().unwrap_or("default");
        let identity = format!("{}:{}:{sense}", scope.as_str(), entry.word);
        Some(Self {
            reference: ProgramRef {
                id: Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()),
                version: 1,
            },
            name: entry.word.clone(),
            language: ProgramLanguage::Forth,
            source_hash: hash_text(&source),
            source,
            documentation: entry.definition.clone(),
            signature: entry.stack_effect.clone(),
            effect: entry
                .effect
                .as_deref()
                .and_then(|effect| effect.parse().ok())
                .unwrap_or(ExecutionEffect::Unclassified),
            capabilities: Vec::new(),
            dependencies: Vec::new(),
            tests: entry
                .proof
                .iter()
                .map(|parts| format!("{} == {}", parts[0], parts[1]))
                .chain(
                    entry
                        .claim
                        .iter()
                        .map(|parts| format!("{} ~ {} when {}", parts[0], parts[1], parts[2])),
                )
                .collect(),
            provenance: "forth-library".to_string(),
            trust: TrustState::Approved,
            scope,
            scope_key: None,
            environment_hash: "forth-library".to_string(),
        })
    }

    /// Project a persisted top-level Lisp `define` expression into the registry.
    pub fn from_lisp_define(source: &str, scope_key: Option<String>) -> Option<Self> {
        let (name, signature) = lisp_definition_identity(source)?;
        let identity = format!("personal:lisp:{name}");
        Some(Self {
            reference: ProgramRef {
                id: Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()),
                version: 0,
            },
            name,
            language: ProgramLanguage::Lisp,
            source: source.to_string(),
            documentation: lisp_definition_documentation(source)
                .filter(|documentation| !documentation.is_empty())
                .unwrap_or_else(|| "Persisted Lisp definition".to_string()),
            signature,
            effect: declared_effect(source, ProgramLanguage::Lisp)
                .unwrap_or(ExecutionEffect::Unclassified),
            capabilities: Vec::new(),
            dependencies: Vec::new(),
            tests: Vec::new(),
            provenance: "lisp-repl".to_string(),
            trust: TrustState::Candidate,
            scope: ProgramScope::Personal,
            scope_key,
            source_hash: hash_text(source),
            environment_hash: "lisp-env".to_string(),
        })
    }

    /// Load one plain-text `.forth` or `.lisp` file as a canonical definition.
    pub fn from_source_file(path: &Path, root: &Path, scope: ProgramScope) -> Result<Self> {
        const MAX_PROGRAM_SOURCE_BYTES: u64 = 1024 * 1024;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to inspect program source {}", path.display()))?;
        if metadata.len() > MAX_PROGRAM_SOURCE_BYTES {
            bail!("program source exceeds 1 MiB: {}", path.display());
        }
        let language = match path.extension().and_then(|extension| extension.to_str()) {
            Some("forth") => ProgramLanguage::Forth,
            Some("lisp") => ProgramLanguage::Lisp,
            _ => bail!("unsupported program source extension: {}", path.display()),
        };
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read program source {}", path.display()))?;
        let fallback_name = path
            .strip_prefix(root)
            .unwrap_or(path)
            .with_extension("")
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let (name, signature) = match language {
            ProgramLanguage::Forth => forth_definition_identity(&source)
                .map(|(name, signature)| (name, signature))
                .unwrap_or((fallback_name, None)),
            ProgramLanguage::Lisp => {
                lisp_definition_identity(&source).unwrap_or((fallback_name, None))
            }
        };
        let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        let identity = format!("{}:{}:{relative}", scope.as_str(), root.display());
        let effect = declared_effect(&source, language).unwrap_or(ExecutionEffect::Unclassified);
        Ok(Self {
            reference: ProgramRef {
                id: Uuid::new_v5(&Uuid::NAMESPACE_URL, identity.as_bytes()),
                version: 0,
            },
            name,
            language,
            source_hash: hash_text(&source),
            documentation: leading_documentation(&source, language),
            source,
            signature,
            effect,
            capabilities: Vec::new(),
            dependencies: Vec::new(),
            tests: Vec::new(),
            provenance: path.display().to_string(),
            trust: match scope {
                ProgramScope::Imported => TrustState::Quarantined,
                ProgramScope::Session | ProgramScope::Task => TrustState::Candidate,
                _ => TrustState::Approved,
            },
            scope,
            scope_key: Some(root.display().to_string()),
            environment_hash: hash_text(&root.display().to_string()),
        })
    }
}

/// Discover canonical plain-text programs below a vocabulary directory.
pub fn load_program_files(root: &Path, scope: ProgramScope) -> Result<Vec<ProgramDefinition>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("forth" | "lisp")
            )
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| ProgramDefinition::from_source_file(&path, root, scope))
        .collect()
}

/// Locate `<git-root>/vocabulary/programs` for the current project.
pub fn project_program_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(|root| root.join("vocabulary").join("programs"))
}

/// Compact definition supplied to an LLM during the VM handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramSummary {
    pub reference: ProgramRef,
    pub name: String,
    pub language: ProgramLanguage,
    pub documentation: String,
    pub signature: Option<String>,
    pub effect: ExecutionEffect,
    pub trust: TrustState,
}

impl From<&ProgramDefinition> for ProgramSummary {
    fn from(definition: &ProgramDefinition) -> Self {
        Self {
            reference: definition.reference.clone(),
            name: definition.name.clone(),
            language: definition.language,
            documentation: definition.documentation.clone(),
            signature: definition.signature.clone(),
            effect: definition.effect,
            trust: definition.trust,
        }
    }
}

/// Compact runtime discovery document refreshed across model/session changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmManifest {
    pub protocol_version: u32,
    pub registry_generation: u64,
    pub environment_hash: String,
    pub languages: Vec<ProgramLanguage>,
    #[serde(default)]
    pub language_packages: Vec<LanguagePackageIdentity>,
    pub core_effects: Vec<String>,
    pub relevant_programs: Vec<ProgramSummary>,
}

impl VmManifest {
    /// Format a deliberately compact block suitable for prompt injection.
    pub fn prompt_block(&self) -> String {
        let mut lines = vec![
            BOOT_CAPSULE.trim().to_string(),
            format!(
                "Finch VM manifest v{} generation={} environment={}",
                self.protocol_version, self.registry_generation, self.environment_hash
            ),
        ];
        lines.push("Languages: forth, lisp".to_string());
        if !self.language_packages.is_empty() {
            let packages = self
                .language_packages
                .iter()
                .map(|package| {
                    format!(
                        "{}@{}#{}",
                        package.name,
                        package.version,
                        &package.sha256[..12]
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Language packages: {packages}"));
        }
        lines.push(format!("Effects: {}", self.core_effects.join(", ")));
        if !self.relevant_programs.is_empty() {
            lines.push("Relevant vocabulary:".to_string());
            for program in &self.relevant_programs {
                let signature = program.signature.as_deref().unwrap_or("signature unknown");
                lines.push(format!(
                    "- {} [{} {} effect={}]: {}",
                    program.name,
                    program.language.as_str(),
                    signature,
                    program.effect.as_str(),
                    program.documentation
                ));
            }
        }
        lines.push(
            "Use vocabulary introspection for exact source; never assume a remembered definition."
                .to_string(),
        );
        lines.join("\n")
    }
}

/// SHA-256 of canonical source or environment material.
pub fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn lisp_definition_identity(source: &str) -> Option<(String, Option<String>)> {
    use crate::lisp::Val;
    let expression = crate::lisp::reader::parse_str(source)
        .ok()?
        .into_iter()
        .next()?;
    let Val::List(parts) = expression else {
        return None;
    };
    if parts.first() != Some(&Val::Symbol("define".to_string())) {
        return None;
    }
    match parts.get(1)? {
        Val::Symbol(name) => Some((name.clone(), None)),
        Val::List(head) => {
            let Val::Symbol(name) = head.first()? else {
                return None;
            };
            let arity = head.len().saturating_sub(1);
            Some((name.clone(), Some(format!("({arity} args -> value)"))))
        }
        _ => None,
    }
}

/// Return a Python/Common-Lisp-style docstring from a typed Finch `define`.
/// The typed compiler treats the first body string as metadata and omits it
/// from the emitted IR, so this parser deliberately follows the same rule.
fn lisp_definition_documentation(source: &str) -> Option<String> {
    use crate::lisp::Val;
    let expression = crate::lisp::reader::parse_str(source)
        .ok()?
        .into_iter()
        .next()?;
    let Val::List(parts) = expression else {
        return None;
    };
    if parts.first() != Some(&Val::Symbol("define".to_string())) {
        return None;
    }
    let mut body_start = if parts.get(2) == Some(&Val::Symbol(":".to_string())) {
        4
    } else {
        2
    };
    if parts.get(body_start) == Some(&Val::Symbol("!".to_string())) {
        body_start += 2;
    }
    match parts.get(body_start) {
        Some(Val::Str(documentation)) => Some(documentation.clone()),
        _ => None,
    }
}

fn forth_definition_identity(source: &str) -> Option<(String, Option<String>)> {
    let tokens = crate::coforth::tokenize(source);
    let colon = tokens.iter().position(|token| token == ":")?;
    let name = tokens.get(colon + 1)?.clone();
    let signature = source.lines().find_map(|line| {
        let start = line.find('(')?;
        let end = line[start..].find(')')? + start;
        let candidate = &line[start..=end];
        candidate.contains("--").then(|| candidate.to_string())
    });
    Some((name, signature))
}

fn leading_documentation(source: &str, language: ProgramLanguage) -> String {
    source
        .lines()
        .map(str::trim)
        .take_while(|line| {
            line.is_empty()
                || match language {
                    ProgramLanguage::Forth => line.starts_with('\\'),
                    ProgramLanguage::Lisp => line.starts_with(';'),
                }
        })
        .filter_map(|line| {
            let text = match language {
                ProgramLanguage::Forth => line.strip_prefix('\\'),
                ProgramLanguage::Lisp => line.strip_prefix(';'),
            }?;
            let text = text.trim();
            if text.is_empty() || text.starts_with("finch-effect:") {
                return None;
            }
            // `finch-doc:` is the explicit, portable comment spelling for a
            // self-contained script.  Store its payload, not the protocol
            // marker, so a registry/manifest consumer sees the same prose as
            // it would for a Lisp definition docstring.
            if let Some(documentation) = text.strip_prefix("finch-doc:") {
                let documentation = documentation.trim();
                (!documentation.is_empty()).then(|| documentation.to_string())
            } else {
                Some(text.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn declared_effect(source: &str, language: ProgramLanguage) -> Option<ExecutionEffect> {
    use std::str::FromStr;

    source.lines().take(16).find_map(|line| {
        let line = line.trim();
        let comment = match language {
            ProgramLanguage::Forth => line.strip_prefix('\\'),
            ProgramLanguage::Lisp => line.strip_prefix(';'),
        }?;
        let value = comment.trim().strip_prefix("finch-effect:")?.trim();
        ExecutionEffect::from_str(value).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_language_uses_compact_wire_inference() {
        assert_eq!(
            ProgramLanguage::infer_source("  (say \"hi\")"),
            ProgramLanguage::Lisp
        );
        assert_eq!(
            ProgramLanguage::infer_source("s\"hi\" say"),
            ProgramLanguage::Forth
        );
    }

    #[test]
    fn compact_wire_inference_rejects_markdown_wrappers() {
        let error = ProgramLanguage::infer_wire_source("```forth\ns\"hi\" say\n```").unwrap_err();
        assert!(error.to_string().contains("E-WIRE-002"));
        let error = ProgramLanguage::infer_wire_source("  \n").unwrap_err();
        assert!(error.to_string().contains("E-WIRE-001"));
        assert_eq!(
            ProgramLanguage::infer_wire_source("  (say \"hi\")").unwrap(),
            ProgramLanguage::Lisp
        );
    }

    #[test]
    fn executable_lisp_script_strips_the_shebang_and_forces_its_language() {
        let script = parse_finch_script(
            Path::new("rebuild.finch"),
            "#!/usr/local/finch --exec --language=lisp\n(begin (say \"ready\"))\n",
        )
        .unwrap();
        assert_eq!(script.language, ProgramLanguage::Lisp);
        assert_eq!(script.source, "(begin (say \"ready\"))\n");
    }

    #[test]
    fn executable_script_uses_extension_or_compact_inference_when_unpinned() {
        let forth = parse_finch_script(
            Path::new("double.forth"),
            "#!/usr/local/bin/finch --exec\n2 *\n",
        )
        .unwrap();
        assert_eq!(forth.language, ProgramLanguage::Forth);

        let lisp = parse_finch_script(
            Path::new("unnamed"),
            "#!/usr/bin/env finch --exec\n(+ 2 3)\n",
        )
        .unwrap();
        assert_eq!(lisp.language, ProgramLanguage::Lisp);

        let windows = parse_finch_script(
            Path::new("script.lisp"),
            "#!C:\\finch\\finch --exec\r\n(+ 2 3)\r\n",
        )
        .unwrap();
        assert_eq!(windows.language, ProgramLanguage::Lisp);
        assert_eq!(windows.source, "(+ 2 3)\r\n");
    }

    #[test]
    fn executable_script_requires_exec_mode_but_not_a_redundant_interpreter_check() {
        assert!(
            parse_finch_script(Path::new("bad"), "#!/usr/local/bin/finch\n(+ 2 3)\n",).is_err()
        );
        let direct = parse_finch_script(
            Path::new("direct.lisp"),
            "#!C:\\Program Files\\Finch\\finch --exec\r\n(+ 2 3)\r\n",
        )
        .unwrap();
        assert_eq!(direct.language, ProgramLanguage::Lisp);
    }

    #[test]
    fn vocabulary_scopes_cover_model_promotion_lifecycle() {
        for scope in [
            ProgramScope::Task,
            ProgramScope::Session,
            ProgramScope::Project,
            ProgramScope::Personal,
            ProgramScope::User,
            ProgramScope::Published,
        ] {
            assert_eq!(scope.as_str().parse::<ProgramScope>().unwrap(), scope);
        }
        let task = ProgramDefinition::candidate("task-word", ProgramLanguage::Forth, "1");
        assert_eq!(task.scope, ProgramScope::Session);
        assert_eq!(task.trust, TrustState::Candidate);
    }

    #[test]
    fn test_manifest_prompt_omits_program_source() {
        let definition =
            ProgramDefinition::candidate("secret-helper", ProgramLanguage::Forth, "12345 67890 +");
        let manifest = VmManifest {
            protocol_version: MANIFEST_PROTOCOL_VERSION,
            registry_generation: 2,
            environment_hash: "abc".to_string(),
            languages: vec![ProgramLanguage::Forth, ProgramLanguage::Lisp],
            language_packages: language_package_identities(),
            core_effects: vec!["say".to_string()],
            relevant_programs: vec![ProgramSummary::from(&definition)],
        };
        let prompt = manifest.prompt_block();
        assert!(prompt.contains("secret-helper"));
        assert!(!prompt.contains("12345"));
        assert!(prompt.contains("otherwise treats the source as Forth"));
        assert!(prompt.contains("get_language_definition"));
        assert!(prompt.contains("s\"response\" say"));
        assert!(prompt.contains("s\"\"\"text\"\"\""));
        assert!(prompt.contains("if-some ... else ... then"));
        assert!(prompt.contains(": factorial ( S int -- S int ! {} )"));
        assert!(prompt.contains("begin condition while ... repeat"));
        assert!(prompt.contains("[1, 2, 3]"));
        assert!(prompt.contains("{\"first name\":\"Ada\"}"));
        assert!(prompt.contains("Language packages: boot@FINCH-VM-TYPED/1#"));
        assert!(!prompt.contains(".\" response\""));
    }

    #[test]
    fn forth_wire_buffer_emits_only_complete_tokens_across_arbitrary_fragments() {
        let mut buffer = ForthWireBuffer::default();
        assert!(buffer.push("s\"hello").unwrap().is_empty());
        assert_eq!(
            buffer.push(" world\" sa").unwrap(),
            vec![ForthWireToken {
                start_byte: 0,
                end_byte: 14,
                source: "s\"hello world\"".into(),
            }]
        );
        assert_eq!(
            buffer.push("y \\ a partial").unwrap(),
            vec![ForthWireToken {
                start_byte: 15,
                end_byte: 18,
                source: "say".into(),
            }]
        );
        assert_eq!(
            buffer.push(" comment\n42 ").unwrap(),
            vec![ForthWireToken {
                start_byte: 39,
                end_byte: 41,
                source: "42".into(),
            }]
        );
        assert!(buffer.finish().unwrap().is_empty());
    }

    #[test]
    fn forth_wire_buffer_handles_raw_prose_and_rejects_unclosed_literals_at_finish() {
        let mut buffer = ForthWireBuffer::default();
        assert!(buffer.push("s\"\"\"Hello \"").unwrap().is_empty());
        assert_eq!(
            buffer.push("human\"\"\" ").unwrap()[0].source,
            "s\"\"\"Hello \"human\"\"\""
        );

        let mut unterminated = ForthWireBuffer::default();
        unterminated.push("s\"unfinished").unwrap();
        assert!(unterminated
            .finish()
            .unwrap_err()
            .to_string()
            .contains("unterminated"));
    }

    #[test]
    fn forth_wire_buffer_keeps_standard_dot_quote_output_literal_atomic() {
        let mut buffer = ForthWireBuffer::default();
        assert!(buffer.push(".\" hello").unwrap().is_empty());
        assert_eq!(
            buffer.push(" world\" ").unwrap(),
            vec![ForthWireToken {
                start_byte: 0,
                end_byte: 15,
                source: ".\" hello world\"".into(),
            }]
        );

        let mut unterminated = ForthWireBuffer::default();
        unterminated.push(".\" unfinished").unwrap();
        assert!(unterminated
            .finish()
            .unwrap_err()
            .to_string()
            .contains("unterminated"));
    }

    #[test]
    fn test_plain_text_program_files_are_discoverable() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("programs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("double.forth"),
            "\\ Double an integer.\n: double ( n -- n ) 2 * ;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("triple.lisp"),
            "; Triple a number.\n(define (triple x) (* x 3))\n",
        )
        .unwrap();
        let definitions = load_program_files(&root, ProgramScope::Project).unwrap();
        assert_eq!(definitions.len(), 2);
        assert!(definitions.iter().any(|definition| {
            definition.name == "double"
                && definition.documentation == "Double an integer."
                && definition.signature.as_deref() == Some("( n -- n )")
        }));
        assert!(definitions.iter().any(|definition| {
            definition.name == "triple" && definition.documentation == "Triple a number."
        }));
    }

    #[test]
    fn test_effect_is_declared_in_language_comment() {
        let forth = ProgramDefinition::candidate(
            "double",
            ProgramLanguage::Forth,
            "\\ finch-effect: pure\n: double 2 * ;",
        );
        let lisp = ProgramDefinition::candidate(
            "files",
            ProgramLanguage::Lisp,
            "; finch-effect: workspace_read\n(define (files root) root)",
        );
        assert_eq!(forth.effect, ExecutionEffect::Pure);
        assert_eq!(lisp.effect, ExecutionEffect::WorkspaceRead);
        assert!(forth.effect.runs_autonomously());
    }

    #[test]
    fn persisted_lisp_definition_uses_first_body_string_as_docstring() {
        let definition = ProgramDefinition::from_lisp_define(
            "(define (double (n : int)) : int \"Return twice n.\" (* n 2))",
            None,
        )
        .unwrap();
        assert_eq!(definition.documentation, "Return twice n.");
    }

    #[test]
    fn persisted_lisp_definition_skips_effect_annotation_before_docstring() {
        let definition = ProgramDefinition::from_lisp_define(
            "(define (announce (text : string)) : unit ! (session.emit) \
                \"Emit text to the active response.\" (say text))",
            None,
        )
        .unwrap();
        assert_eq!(
            definition.documentation,
            "Emit text to the active response."
        );
    }

    #[test]
    fn self_contained_forth_finch_doc_marker_is_not_part_of_metadata() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("programs");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("double.forth");
        std::fs::write(
            &path,
            "\\ finch-doc: Return twice an integer.\n: double ( S int -- S int ! {} ) 2 * ;\n",
        )
        .unwrap();

        let definition =
            ProgramDefinition::from_source_file(&path, &root, ProgramScope::Project).unwrap();
        assert_eq!(definition.documentation, "Return twice an integer.");
    }
}
