//! Caller-owned adapter from program identity onto memory's program index.
//!
//! Memory persists opaque `ProgramIndexRecord` rows. This module maps those
//! rows to `ProgramDefinition`, writes canonical source files, and builds
//! VM discovery manifests. It is composition glue, not a memory or programs
//! implementation detail.

use crate::programs::{
    hash_text, language_package_identities, ExecutionEffect, ProgramDefinition, ProgramLanguage,
    ProgramRef, ProgramScope, ProgramSummary, TrustState, VmManifest, MANIFEST_PROTOCOL_VERSION,
};
use anyhow::{Context, Result};
use finch_memory::{MemorySystem, ProgramIndexRecord, ProgramIndexRef};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Program-registry operations that composition owns.
pub struct ProgramRegistry {
    memory: Arc<MemorySystem>,
}

impl ProgramRegistry {
    /// Wrap an opened memory store.
    pub fn new(memory: Arc<MemorySystem>) -> Self {
        Self { memory }
    }

    /// Wrap a borrowed `Arc` without taking ownership of the caller's handle.
    pub fn from_ref(memory: &Arc<MemorySystem>) -> Self {
        Self {
            memory: Arc::clone(memory),
        }
    }

    /// Write an authored definition to the browsable vocabulary first, then index it.
    ///
    /// The source file is canonical. SQLite is a disposable discovery and usage cache.
    pub async fn save_authored_program(
        &self,
        definition: ProgramDefinition,
    ) -> Result<(ProgramRef, PathBuf)> {
        let root = self.memory.program_source_root();
        let authored_root = if definition.scope == ProgramScope::Personal {
            root.join("generated")
        } else {
            root.join("generated").join(definition.scope.as_str())
        };
        std::fs::create_dir_all(&authored_root).with_context(|| {
            format!(
                "failed to create authored vocabulary directory {}",
                authored_root.display()
            )
        })?;

        let extension = definition.language.as_str();
        let filename = format!("{}.{extension}", safe_program_filename(&definition.name));
        let path = authored_root.join(filename);
        write_program_source(&path, &definition.source)?;

        let mut indexed = ProgramDefinition::from_source_file(&path, &root, definition.scope)?;
        indexed.documentation = definition.documentation;
        indexed.signature = definition.signature.or(indexed.signature);
        indexed.effect = definition.effect;
        indexed.capabilities = definition.capabilities;
        indexed.dependencies = definition.dependencies;
        indexed.tests = definition.tests;
        indexed.provenance = path.display().to_string();
        indexed.trust = definition.trust;
        indexed.scope = definition.scope;
        indexed.scope_key = definition.scope_key;
        indexed.environment_hash = definition.environment_hash;
        let reference = self.index_program_definition(indexed).await?;
        Ok((reference, path))
    }

    /// Persist a Lisp `(define ...)` and project it into the authored registry.
    pub async fn save_lisp_define(&self, expr: &str) -> Result<()> {
        self.memory.save_lisp_define(expr).await?;
        if let Some(definition) = ProgramDefinition::from_lisp_define(expr, None) {
            self.save_authored_program(definition).await?;
        }
        Ok(())
    }

    /// Update the rebuildable SQLite projection for one source-backed definition.
    pub async fn index_program_definition(
        &self,
        definition: ProgramDefinition,
    ) -> Result<ProgramRef> {
        let record = record_from_definition(&definition)?;
        let stored = self.memory.index_program_record(record).await?;
        reference_from_index(&stored)
    }

    /// Look up one immutable program version.
    pub async fn get_program_definition(
        &self,
        reference: &ProgramRef,
    ) -> Result<Option<ProgramDefinition>> {
        let record = self
            .memory
            .get_program_index(&reference.id.to_string(), reference.version)
            .await?;
        record.map(definition_from_record).transpose()
    }

    /// Resolve the newest non-deprecated version of a scoped program name.
    pub async fn get_program_by_name(
        &self,
        name: &str,
        language: Option<ProgramLanguage>,
    ) -> Result<Option<ProgramDefinition>> {
        let language = language.map(|language| language.as_str().to_string());
        let record = self
            .memory
            .get_program_index_by_name(name, language.as_deref())
            .await?;
        record.map(definition_from_record).transpose()
    }

    /// Search current program versions using a compact lexical relevance score.
    pub async fn search_program_definitions(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProgramDefinition>> {
        self.memory
            .search_program_indexes(query, limit)
            .await?
            .into_iter()
            .map(definition_from_record)
            .collect()
    }

    /// Current monotonic registry generation used to invalidate stale model manifests.
    pub async fn program_registry_generation(&self) -> Result<u64> {
        self.memory.program_registry_generation().await
    }

    /// Build a compact discovery manifest for a model and its current task.
    pub async fn vm_manifest(&self, query: &str, limit: usize) -> Result<VmManifest> {
        let generation = self.program_registry_generation().await?;
        let relevant_programs = self
            .search_program_definitions(query, limit)
            .await?
            .iter()
            .map(ProgramSummary::from)
            .collect();
        Ok(VmManifest {
            protocol_version: MANIFEST_PROTOCOL_VERSION,
            registry_generation: generation,
            environment_hash: hash_text(&format!("finch-registry:{generation}")),
            languages: vec![ProgramLanguage::Forth, ProgramLanguage::Lisp],
            language_packages: language_package_identities(),
            core_effects: vec![
                "say".to_string(),
                "show_dialog".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
                "execute_process".to_string(),
                "invoke_program".to_string(),
                "send_to_peer".to_string(),
            ],
            relevant_programs,
        })
    }

    /// Load canonical `.forth` and `.lisp` files and update the searchable index.
    pub async fn sync_program_files(&self, root: &Path, scope: ProgramScope) -> Result<usize> {
        let records = crate::programs::load_program_files(root, scope)?
            .iter()
            .map(record_from_definition)
            .collect::<Result<Vec<_>>>()?;
        self.memory.index_program_records(records).await
    }
}

fn record_from_definition(definition: &ProgramDefinition) -> Result<ProgramIndexRecord> {
    Ok(ProgramIndexRecord {
        id: definition.reference.id.to_string(),
        version: definition.reference.version,
        name: definition.name.clone(),
        language: definition.language.as_str().to_string(),
        source: definition.source.clone(),
        documentation: definition.documentation.clone(),
        signature: definition.signature.clone(),
        effect: definition.effect.as_str().to_string(),
        capabilities_json: serde_json::to_string(&definition.capabilities)?,
        dependencies_json: serde_json::to_string(&definition.dependencies)?,
        tests_json: serde_json::to_string(&definition.tests)?,
        provenance: definition.provenance.clone(),
        trust: definition.trust.as_str().to_string(),
        scope: definition.scope.as_str().to_string(),
        scope_key: definition.scope_key.clone(),
        source_hash: definition.source_hash.clone(),
        environment_hash: definition.environment_hash.clone(),
    })
}

fn definition_from_record(record: ProgramIndexRecord) -> Result<ProgramDefinition> {
    Ok(ProgramDefinition {
        reference: ProgramRef {
            id: Uuid::parse_str(&record.id).context("invalid program ID in registry")?,
            version: record.version,
        },
        name: record.name,
        language: ProgramLanguage::from_str(&record.language)?,
        source: record.source,
        documentation: record.documentation,
        signature: record.signature,
        effect: ExecutionEffect::from_str(&record.effect)?,
        capabilities: serde_json::from_str(&record.capabilities_json)?,
        dependencies: serde_json::from_str(&record.dependencies_json)?,
        tests: serde_json::from_str(&record.tests_json)?,
        provenance: record.provenance,
        trust: TrustState::from_str(&record.trust)?,
        scope: ProgramScope::from_str(&record.scope)?,
        scope_key: record.scope_key,
        source_hash: record.source_hash,
        environment_hash: record.environment_hash,
    })
}

fn reference_from_index(stored: &ProgramIndexRef) -> Result<ProgramRef> {
    Ok(ProgramRef {
        id: Uuid::parse_str(&stored.id).context("invalid program ID in registry")?,
        version: stored.version,
    })
}

fn safe_program_filename(name: &str) -> String {
    let mut filename = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            filename.push(character.to_ascii_lowercase());
        } else {
            filename.push('-');
        }
    }
    let filename = filename.trim_matches('-');
    if filename.is_empty() {
        format!("program-{}", &hash_text(name)[..12])
    } else {
        filename.to_string()
    }
}

fn write_program_source(path: &Path, source: &str) -> Result<()> {
    let mut normalized = source.to_string();
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    std::fs::write(path, normalized)
        .with_context(|| format!("failed to write program source {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use finch_memory::MemoryConfig;
    use tempfile::TempDir;

    fn registry(temp: &TempDir) -> ProgramRegistry {
        ProgramRegistry::new(Arc::new(
            MemorySystem::new(MemoryConfig {
                db_path: temp.path().join("memory.db"),
                enabled: true,
                use_neural_embeddings: false,
                ..MemoryConfig::default()
            })
            .unwrap(),
        ))
    }

    #[tokio::test]
    async fn test_program_survives_registry_reopen() {
        let temp = TempDir::new().unwrap();
        let reference = {
            let registry = registry(&temp);
            registry
                .index_program_definition(ProgramDefinition::candidate(
                    "double",
                    ProgramLanguage::Forth,
                    ": double 2 * ;",
                ))
                .await
                .unwrap()
        };
        let reopened = registry(&temp);
        let definition = reopened
            .get_program_definition(&reference)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(definition.name, "double");
        assert_eq!(definition.source, ": double 2 * ;");
    }

    #[tokio::test]
    async fn test_program_revision_increments_generation_and_version() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let first = registry
            .index_program_definition(ProgramDefinition::candidate(
                "double",
                ProgramLanguage::Forth,
                ": double 2 * ;",
            ))
            .await
            .unwrap();
        let second = registry
            .index_program_definition(ProgramDefinition::candidate(
                "double",
                ProgramLanguage::Forth,
                ": double dup + ;",
            ))
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.version, first.version + 1);
        assert_eq!(registry.program_registry_generation().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_manifest_rediscovers_program_without_source() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        registry
            .index_program_definition(ProgramDefinition::candidate(
                "test-changes",
                ProgramLanguage::Lisp,
                "(define (test-changes paths) (length paths))",
            ))
            .await
            .unwrap();
        let manifest = registry.vm_manifest("test changed files", 5).await.unwrap();
        assert_eq!(manifest.registry_generation, 1);
        assert!(manifest
            .relevant_programs
            .iter()
            .any(|program| program.name == "test-changes"));
        assert!(!manifest
            .prompt_block()
            .contains("(define (test-changes paths) (length paths))"));
    }

    #[tokio::test]
    async fn test_saved_lisp_define_is_projected_into_registry() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        registry
            .save_lisp_define("(define (triple x) (* x 3))")
            .await
            .unwrap();
        let definition = registry
            .get_program_by_name("triple", Some(ProgramLanguage::Lisp))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(definition.signature.as_deref(), Some("(1 args -> value)"));
        let source_path = temp
            .path()
            .join("vocabulary/programs/generated/triple.lisp");
        assert_eq!(
            std::fs::read_to_string(source_path).unwrap(),
            "(define (triple x) (* x 3))\n"
        );
    }

    #[tokio::test]
    async fn authored_program_preserves_promotion_scope() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let mut definition = ProgramDefinition::candidate(
            "project-helper",
            ProgramLanguage::Forth,
            ": project-helper 1 ;",
        );
        definition.scope = ProgramScope::Project;
        definition.scope_key = Some("workspace-alpha".into());
        registry.save_authored_program(definition).await.unwrap();
        let stored = registry
            .get_program_by_name("project-helper", Some(ProgramLanguage::Forth))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.scope, ProgramScope::Project);
        assert_eq!(stored.scope_key.as_deref(), Some("workspace-alpha"));
        assert!(temp
            .path()
            .join("vocabulary/programs/generated/project/project-helper.forth")
            .exists());
    }

    #[test]
    fn test_program_filenames_are_safe_and_readable() {
        assert_eq!(safe_program_filename("Show Rust Files"), "show-rust-files");
        assert!(safe_program_filename("!!!").starts_with("program-"));
    }
}
