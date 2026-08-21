//! Persistent, language-neutral program vocabulary.
//!
//! Forth and Lisp keep their native evaluators. This module gives definitions a
//! shared identity, metadata model, discovery manifest, and pure invocation ABI.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Version of the model/runtime vocabulary handshake.
pub const MANIFEST_PROTOCOL_VERSION: u32 = 1;

/// Minimal language/runtime definition supplied to every fresh model context.
pub const BOOT_CAPSULE: &str = include_str!("../../vocabulary/BOOT.md");

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

/// Persistence and visibility boundary for a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramScope {
    Builtin,
    Session,
    Project,
    Personal,
    Imported,
}

impl ProgramScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Session => "session",
            Self::Project => "project",
            Self::Personal => "personal",
            Self::Imported => "imported",
        }
    }
}

impl std::str::FromStr for ProgramScope {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "personal" => Ok(Self::Personal),
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
    String(String),
    Bytes(Vec<u8>),
    List(Vec<ProgramValue>),
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
            documentation: "Persisted Lisp definition".to_string(),
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
                ProgramScope::Session => TrustState::Candidate,
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

/// Invoke a native Forth definition through the shared pure-value ABI.
pub fn invoke_forth(
    definition: &ProgramDefinition,
    args: &[ProgramValue],
) -> Result<Vec<ProgramValue>> {
    if definition.language != ProgramLanguage::Forth {
        bail!("{} is not a Forth program", definition.name);
    }
    let mut vm = crate::coforth::Library::precompiled_vm();
    let ints = args
        .iter()
        .map(|value| match value {
            ProgramValue::Int(value) => Ok(*value),
            other => bail!("initial Forth ABI accepts integers, got {other:?}"),
        })
        .collect::<Result<Vec<_>>>()?;
    vm.push_stack(&ints);
    if definition.source.trim_start().starts_with(':') {
        vm.exec(&definition.source)
            .with_context(|| format!("failed to compile {}", definition.name))?;
        vm.exec(&definition.name)
            .with_context(|| format!("failed to invoke {}", definition.name))?;
    } else {
        vm.exec(&definition.source)
            .with_context(|| format!("failed to invoke {}", definition.name))?;
    }
    Ok(vm
        .data_stack()
        .iter()
        .copied()
        .map(ProgramValue::Int)
        .collect())
}

/// Invoke a Lisp definition through the shared pure-value ABI.
pub async fn invoke_lisp(
    definition: &ProgramDefinition,
    args: &[ProgramValue],
) -> Result<Vec<ProgramValue>> {
    if definition.language != ProgramLanguage::Lisp {
        bail!("{} is not a Lisp program", definition.name);
    }
    let values = args.iter().cloned().map(program_to_lisp).collect();
    let value = crate::lisp::invoke_source(&definition.name, &definition.source, values).await?;
    Ok(match value {
        crate::lisp::Val::Nil => Vec::new(),
        crate::lisp::Val::List(items) => items
            .into_iter()
            .map(lisp_to_program)
            .collect::<Result<Vec<_>>>()?,
        other => vec![lisp_to_program(other)?],
    })
}

fn program_to_lisp(value: ProgramValue) -> crate::lisp::Val {
    match value {
        ProgramValue::Nil => crate::lisp::Val::Nil,
        ProgramValue::Bool(value) => crate::lisp::Val::Bool(value),
        ProgramValue::Int(value) => crate::lisp::Val::Int(value),
        ProgramValue::Float(value) => crate::lisp::Val::Float(value),
        ProgramValue::String(value) => crate::lisp::Val::Str(value),
        ProgramValue::Bytes(value) => crate::lisp::Val::Bytes(value),
        ProgramValue::List(values) => {
            crate::lisp::Val::List(values.into_iter().map(program_to_lisp).collect())
        }
    }
}

fn lisp_to_program(value: crate::lisp::Val) -> Result<ProgramValue> {
    match value {
        crate::lisp::Val::Nil => Ok(ProgramValue::Nil),
        crate::lisp::Val::Bool(value) => Ok(ProgramValue::Bool(value)),
        crate::lisp::Val::Int(value) => Ok(ProgramValue::Int(value)),
        crate::lisp::Val::Float(value) => Ok(ProgramValue::Float(value)),
        crate::lisp::Val::Str(value) | crate::lisp::Val::Symbol(value) => {
            Ok(ProgramValue::String(value))
        }
        crate::lisp::Val::Bytes(value) => Ok(ProgramValue::Bytes(value)),
        crate::lisp::Val::List(values) => Ok(ProgramValue::List(
            values
                .into_iter()
                .map(lisp_to_program)
                .collect::<Result<Vec<_>>>()?,
        )),
        other => bail!("Lisp value is not portable: {other}"),
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
            (!text.is_empty() && !text.starts_with("finch-effect:")).then(|| text.to_string())
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
    fn test_forth_program_invocation_uses_shared_values() {
        let mut definition =
            ProgramDefinition::candidate("double", ProgramLanguage::Forth, ": double 2 * ;");
        definition.signature = Some("( n -- n )".to_string());
        let result = invoke_forth(&definition, &[ProgramValue::Int(21)]).unwrap();
        assert_eq!(result, vec![ProgramValue::Int(42)]);
    }

    #[tokio::test]
    async fn test_lisp_program_invocation_uses_shared_values() {
        let definition = ProgramDefinition::candidate(
            "double",
            ProgramLanguage::Lisp,
            "(define (double x) (* x 2))",
        );
        let result = invoke_lisp(&definition, &[ProgramValue::Int(21)])
            .await
            .unwrap();
        assert_eq!(result, vec![ProgramValue::Int(42)]);
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
            core_effects: vec!["say".to_string()],
            relevant_programs: vec![ProgramSummary::from(&definition)],
        };
        let prompt = manifest.prompt_block();
        assert!(prompt.contains("secret-helper"));
        assert!(!prompt.contains("12345"));
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
}
