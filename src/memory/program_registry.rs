//! SQLite persistence and discovery for the shared Forth/Lisp program registry.

use super::MemorySystem;
use crate::programs::{
    hash_text, language_package_identities, ExecutionEffect, ProgramDefinition, ProgramLanguage,
    ProgramRef, ProgramScope, ProgramSummary, TrustState, VmManifest, MANIFEST_PROTOCOL_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Row, Transaction};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

const GENERATION_KEY: &str = "program_registry_generation";

impl MemorySystem {
    /// Write an authored definition to the browsable vocabulary first, then index it.
    ///
    /// The source file is canonical. SQLite is a disposable discovery and usage cache.
    pub async fn save_authored_program(
        &self,
        definition: ProgramDefinition,
    ) -> Result<(ProgramRef, PathBuf)> {
        let root = self.program_source_root();
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

        let mut indexed =
            ProgramDefinition::from_source_file(&path, &root, definition.scope)?;
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

    /// Root containing user-readable program sources beside the memory database.
    pub fn program_source_root(&self) -> PathBuf {
        self.config
            .db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("vocabulary")
            .join("programs")
    }

    /// Update the rebuildable SQLite projection for one source-backed definition.
    async fn index_program_definition(
        &self,
        mut definition: ProgramDefinition,
    ) -> Result<ProgramRef> {
        let mut conn = self.db.lock().await;
        let tx = conn.transaction()?;
        let (reference, inserted) = upsert_definition(&tx, &mut definition)?;
        if inserted {
            bump_generation(&tx)?;
        }
        tx.commit()?;
        Ok(reference)
    }

    /// Look up one immutable program version.
    pub async fn get_program_definition(
        &self,
        reference: &ProgramRef,
    ) -> Result<Option<ProgramDefinition>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, version, name, language, source, documentation, signature, effect,
                    capabilities_json, dependencies_json, tests_json, provenance, trust,
                    scope, scope_key, source_hash, environment_hash
             FROM program_registry WHERE id = ?1 AND version = ?2",
        )?;
        let mut rows = stmt.query(params![reference.id.to_string(), reference.version])?;
        Ok(rows.next()?.map(row_to_definition).transpose()?)
    }

    /// Resolve the newest non-deprecated version of a scoped program name.
    pub async fn get_program_by_name(
        &self,
        name: &str,
        language: Option<ProgramLanguage>,
    ) -> Result<Option<ProgramDefinition>> {
        let definitions = self.latest_program_definitions().await?;
        Ok(definitions.into_iter().find(|definition| {
            definition.name.eq_ignore_ascii_case(name)
                && language.is_none_or(|wanted| definition.language == wanted)
        }))
    }

    /// Search current program versions using a compact lexical relevance score.
    pub async fn search_program_definitions(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProgramDefinition>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = query.trim().to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().collect();
        let mut definitions = self.latest_program_definitions().await?;
        definitions.sort_by(|a, b| {
            relevance_score(b, &query, &tokens)
                .cmp(&relevance_score(a, &query, &tokens))
                .then_with(|| a.name.cmp(&b.name))
        });
        if !query.is_empty() {
            definitions.retain(|definition| relevance_score(definition, &query, &tokens) > 0);
        }
        definitions.truncate(limit);
        Ok(definitions)
    }

    /// Current monotonic registry generation used to invalidate stale model manifests.
    pub async fn program_registry_generation(&self) -> Result<u64> {
        let conn = self.db.lock().await;
        read_generation(&conn)
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

    /// Project the existing Co-Forth library into the registry in one transaction.
    pub async fn sync_forth_vocabulary(&self, library: &crate::coforth::Library) -> Result<usize> {
        let definitions: Vec<_> = library
            .all_entries()
            .into_iter()
            .filter_map(|entry| ProgramDefinition::from_forth_entry(entry, ProgramScope::Builtin))
            .collect();
        let mut conn = self.db.lock().await;
        let tx = conn.transaction()?;
        let mut inserted = 0;
        for mut definition in definitions {
            if upsert_definition(&tx, &mut definition)?.1 {
                inserted += 1;
            }
        }
        if inserted > 0 {
            bump_generation(&tx)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Load canonical `.forth` and `.lisp` files and update the searchable index.
    pub async fn sync_program_files(
        &self,
        root: &std::path::Path,
        scope: ProgramScope,
    ) -> Result<usize> {
        let definitions = crate::programs::load_program_files(root, scope)?;
        let mut conn = self.db.lock().await;
        let tx = conn.transaction()?;
        let mut inserted = 0;
        for mut definition in definitions {
            if upsert_definition(&tx, &mut definition)?.1 {
                inserted += 1;
            }
        }
        if inserted > 0 {
            bump_generation(&tx)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    async fn latest_program_definitions(&self) -> Result<Vec<ProgramDefinition>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT p.id, p.version, p.name, p.language, p.source, p.documentation,
                    p.signature, p.effect, p.capabilities_json, p.dependencies_json, p.tests_json,
                    p.provenance, p.trust, p.scope, p.scope_key, p.source_hash,
                    p.environment_hash
             FROM program_registry p
             WHERE p.trust != 'deprecated'
               AND p.version = (
                   SELECT MAX(p2.version) FROM program_registry p2 WHERE p2.id = p.id
               )",
        )?;
        let definitions = stmt
            .query_map([], row_to_definition)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read program registry")?;
        Ok(definitions)
    }
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

fn upsert_definition(
    tx: &Transaction<'_>,
    definition: &mut ProgramDefinition,
) -> Result<(ProgramRef, bool)> {
    let scope_key = definition.scope_key.as_deref().unwrap_or("");
    let latest = tx
        .query_row(
            "SELECT id, version, source_hash FROM program_registry
             WHERE name = ?1 AND language = ?2 AND scope = ?3
               AND COALESCE(scope_key, '') = ?4
             ORDER BY version DESC LIMIT 1",
            params![
                definition.name,
                definition.language.as_str(),
                definition.scope.as_str(),
                scope_key,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;

    if let Some((id, version, source_hash)) = latest {
        let id = Uuid::parse_str(&id).context("invalid program ID in registry")?;
        if source_hash == definition.source_hash {
            return Ok((ProgramRef { id, version }, false));
        }
        definition.reference = ProgramRef {
            id,
            version: version + 1,
        };
    } else if definition.reference.version == 0 {
        definition.reference.version = 1;
    }

    let now = chrono::Utc::now().timestamp();
    tx.execute(
        "INSERT INTO program_registry (
             id, version, name, language, source, documentation, signature, effect,
             capabilities_json, dependencies_json, tests_json, provenance, trust, scope,
             scope_key, source_hash, environment_hash, created_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
        params![
            definition.reference.id.to_string(),
            definition.reference.version,
            definition.name,
            definition.language.as_str(),
            definition.source,
            definition.documentation,
            definition.signature,
            definition.effect.as_str(),
            serde_json::to_string(&definition.capabilities)?,
            serde_json::to_string(&definition.dependencies)?,
            serde_json::to_string(&definition.tests)?,
            definition.provenance,
            definition.trust.as_str(),
            definition.scope.as_str(),
            definition.scope_key,
            definition.source_hash,
            definition.environment_hash,
            now,
        ],
    )?;
    Ok((definition.reference.clone(), true))
}

fn row_to_definition(row: &Row<'_>) -> rusqlite::Result<ProgramDefinition> {
    let id: String = row.get(0)?;
    let language: String = row.get(3)?;
    let trust: String = row.get(12)?;
    let scope: String = row.get(13)?;
    let effect: String = row.get(7)?;
    let capabilities: String = row.get(8)?;
    let dependencies: String = row.get(9)?;
    let tests: String = row.get(10)?;
    Ok(ProgramDefinition {
        reference: ProgramRef {
            id: Uuid::parse_str(&id).map_err(conversion_error)?,
            version: row.get(1)?,
        },
        name: row.get(2)?,
        language: ProgramLanguage::from_str(&language)
            .map_err(|error| conversion_message(error.to_string()))?,
        source: row.get(4)?,
        documentation: row.get(5)?,
        signature: row.get(6)?,
        effect: ExecutionEffect::from_str(&effect)
            .map_err(|error| conversion_message(error.to_string()))?,
        capabilities: serde_json::from_str(&capabilities).map_err(conversion_error)?,
        dependencies: serde_json::from_str(&dependencies).map_err(conversion_error)?,
        tests: serde_json::from_str(&tests).map_err(conversion_error)?,
        provenance: row.get(11)?,
        trust: TrustState::from_str(&trust)
            .map_err(|error| conversion_message(error.to_string()))?,
        scope: ProgramScope::from_str(&scope)
            .map_err(|error| conversion_message(error.to_string()))?,
        scope_key: row.get(14)?,
        source_hash: row.get(15)?,
        environment_hash: row.get(16)?,
    })
}

fn conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn conversion_message(message: String) -> rusqlite::Error {
    conversion_error(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

fn relevance_score(definition: &ProgramDefinition, query: &str, tokens: &[&str]) -> usize {
    if query.is_empty() {
        return 1;
    }
    let name = definition.name.to_lowercase();
    let documentation = definition.documentation.to_lowercase();
    let source = definition.source.to_lowercase();
    let mut score = usize::from(name == query) * 100 + usize::from(name.starts_with(query)) * 30;
    for token in tokens {
        score += usize::from(name.contains(token)) * 15;
        score += usize::from(documentation.contains(token)) * 5;
        score += usize::from(source.contains(token));
    }
    score
}

fn read_generation(conn: &rusqlite::Connection) -> Result<u64> {
    let value = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            [GENERATION_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value.and_then(|value| value.parse().ok()).unwrap_or(0))
}

fn bump_generation(tx: &Transaction<'_>) -> Result<u64> {
    let next = read_generation(tx)? + 1;
    tx.execute(
        "INSERT INTO metadata (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            GENERATION_KEY,
            next.to_string(),
            chrono::Utc::now().timestamp()
        ],
    )?;
    Ok(next)
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryConfig;
    use tempfile::TempDir;

    fn memory(temp: &TempDir) -> MemorySystem {
        MemorySystem::new(MemoryConfig {
            db_path: temp.path().join("memory.db"),
            enabled: true,
            use_neural_embeddings: false,
            ..MemoryConfig::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn test_program_survives_registry_reopen() {
        let temp = TempDir::new().unwrap();
        let reference = {
            let memory = memory(&temp);
            memory
                .index_program_definition(ProgramDefinition::candidate(
                    "double",
                    ProgramLanguage::Forth,
                    ": double 2 * ;",
                ))
                .await
                .unwrap()
        };
        let reopened = memory(&temp);
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
        let memory = memory(&temp);
        let first = memory
            .index_program_definition(ProgramDefinition::candidate(
                "double",
                ProgramLanguage::Forth,
                ": double 2 * ;",
            ))
            .await
            .unwrap();
        let second = memory
            .index_program_definition(ProgramDefinition::candidate(
                "double",
                ProgramLanguage::Forth,
                ": double dup + ;",
            ))
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.version, first.version + 1);
        assert_eq!(memory.program_registry_generation().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_manifest_rediscovers_program_without_source() {
        let temp = TempDir::new().unwrap();
        let memory = memory(&temp);
        memory
            .index_program_definition(ProgramDefinition::candidate(
                "test-changes",
                ProgramLanguage::Lisp,
                "(define (test-changes paths) (length paths))",
            ))
            .await
            .unwrap();
        let manifest = memory.vm_manifest("test changed files", 5).await.unwrap();
        assert_eq!(manifest.registry_generation, 1);
        assert!(manifest
            .relevant_programs
            .iter()
            .any(|program| program.name == "test-changes"));
        // The language bootstrap may legitimately document `define` forms;
        // this assertion protects the program registry contract specifically:
        // relevant definitions are listed by compact metadata, not source.
        assert!(!manifest
            .prompt_block()
            .contains("(define (test-changes paths) (length paths))"));
    }

    #[tokio::test]
    async fn test_saved_lisp_define_is_projected_into_registry() {
        let temp = TempDir::new().unwrap();
        let memory = memory(&temp);
        memory
            .save_lisp_define("(define (triple x) (* x 3))")
            .await
            .unwrap();
        let definition = memory
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
        let memory = memory(&temp);
        let mut definition = ProgramDefinition::candidate(
            "project-helper",
            ProgramLanguage::Forth,
            ": project-helper 1 ;",
        );
        definition.scope = ProgramScope::Project;
        definition.scope_key = Some("workspace-alpha".into());
        memory.save_authored_program(definition).await.unwrap();
        let stored = memory
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
