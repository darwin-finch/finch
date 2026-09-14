//! SQLite persistence for the executable-vocabulary index.
//!
//! Memory stores opaque program-index rows. Callers own mapping to
//! `ProgramDefinition` and file-backed vocabulary through the composition
//! adapter.

use super::MemorySystem;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::path::PathBuf;

const GENERATION_KEY: &str = "program_registry_generation";

/// One stored program-index row. Callers map this to a `ProgramDefinition`.
///
/// Fields match the `program_registry` SQLite columns; enums stay strings so
/// memory does not depend on `programs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramIndexRecord {
    pub id: String,
    pub version: u64,
    pub name: String,
    pub language: String,
    pub source: String,
    pub documentation: String,
    pub signature: Option<String>,
    pub effect: String,
    pub capabilities_json: String,
    pub dependencies_json: String,
    pub tests_json: String,
    pub provenance: String,
    pub trust: String,
    pub scope: String,
    pub scope_key: Option<String>,
    pub source_hash: String,
    pub environment_hash: String,
}

/// Identity of one immutable program-index version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramIndexRef {
    pub id: String,
    pub version: u64,
}

impl MemorySystem {
    /// Root containing user-readable program sources beside the memory database.
    pub fn program_source_root(&self) -> PathBuf {
        self.config
            .db_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("vocabulary")
            .join("programs")
    }

    /// Update the rebuildable SQLite projection for one source-backed definition.
    pub async fn index_program_record(
        &self,
        mut record: ProgramIndexRecord,
    ) -> Result<ProgramIndexRef> {
        let mut conn = self.db.lock().await;
        let tx = conn.transaction()?;
        let (reference, inserted) = upsert_record(&tx, &mut record)?;
        if inserted {
            bump_generation(&tx)?;
        }
        tx.commit()?;
        Ok(reference)
    }

    /// Index many records in one transaction. Generation bumps once if any row was new.
    pub async fn index_program_records(&self, records: Vec<ProgramIndexRecord>) -> Result<usize> {
        let mut conn = self.db.lock().await;
        let tx = conn.transaction()?;
        let mut inserted = 0;
        for mut record in records {
            if upsert_record(&tx, &mut record)?.1 {
                inserted += 1;
            }
        }
        if inserted > 0 {
            bump_generation(&tx)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Look up one immutable program-index version.
    pub async fn get_program_index(
        &self,
        id: &str,
        version: u64,
    ) -> Result<Option<ProgramIndexRecord>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, version, name, language, source, documentation, signature, effect,
                    capabilities_json, dependencies_json, tests_json, provenance, trust,
                    scope, scope_key, source_hash, environment_hash
             FROM program_registry WHERE id = ?1 AND version = ?2",
        )?;
        let mut rows = stmt.query(params![id, version])?;
        Ok(rows.next()?.map(row_to_record).transpose()?)
    }

    /// Resolve the newest non-deprecated version of a scoped program name.
    pub async fn get_program_index_by_name(
        &self,
        name: &str,
        language: Option<&str>,
    ) -> Result<Option<ProgramIndexRecord>> {
        let records = self.latest_program_indexes().await?;
        Ok(records.into_iter().find(|record| {
            record.name.eq_ignore_ascii_case(name)
                && language.is_none_or(|wanted| record.language == wanted)
        }))
    }

    /// Search current program-index versions using a compact lexical score.
    pub async fn search_program_indexes(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProgramIndexRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = query.trim().to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().collect();
        let mut records = self.latest_program_indexes().await?;
        records.sort_by(|a, b| {
            relevance_score(b, &query, &tokens)
                .cmp(&relevance_score(a, &query, &tokens))
                .then_with(|| a.name.cmp(&b.name))
        });
        if !query.is_empty() {
            records.retain(|record| relevance_score(record, &query, &tokens) > 0);
        }
        records.truncate(limit);
        Ok(records)
    }

    /// Current monotonic registry generation used to invalidate stale manifests.
    pub async fn program_registry_generation(&self) -> Result<u64> {
        let conn = self.db.lock().await;
        read_generation(&conn)
    }

    /// Load canonical index rows for every current non-deprecated version.
    pub async fn latest_program_indexes(&self) -> Result<Vec<ProgramIndexRecord>> {
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
        let records = stmt
            .query_map([], row_to_record)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read program registry")?;
        Ok(records)
    }
}

fn upsert_record(
    tx: &Transaction<'_>,
    record: &mut ProgramIndexRecord,
) -> Result<(ProgramIndexRef, bool)> {
    let scope_key = record.scope_key.as_deref().unwrap_or("");
    let latest = tx
        .query_row(
            "SELECT id, version, source_hash FROM program_registry
             WHERE name = ?1 AND language = ?2 AND scope = ?3
               AND COALESCE(scope_key, '') = ?4
             ORDER BY version DESC LIMIT 1",
            params![record.name, record.language, record.scope, scope_key],
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
        if source_hash == record.source_hash {
            return Ok((ProgramIndexRef { id, version }, false));
        }
        record.id = id;
        record.version = version + 1;
    } else if record.version == 0 {
        record.version = 1;
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
            record.id,
            record.version,
            record.name,
            record.language,
            record.source,
            record.documentation,
            record.signature,
            record.effect,
            record.capabilities_json,
            record.dependencies_json,
            record.tests_json,
            record.provenance,
            record.trust,
            record.scope,
            record.scope_key,
            record.source_hash,
            record.environment_hash,
            now,
        ],
    )?;
    Ok((
        ProgramIndexRef {
            id: record.id.clone(),
            version: record.version,
        },
        true,
    ))
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<ProgramIndexRecord> {
    Ok(ProgramIndexRecord {
        id: row.get(0)?,
        version: row.get(1)?,
        name: row.get(2)?,
        language: row.get(3)?,
        source: row.get(4)?,
        documentation: row.get(5)?,
        signature: row.get(6)?,
        effect: row.get(7)?,
        capabilities_json: row.get(8)?,
        dependencies_json: row.get(9)?,
        tests_json: row.get(10)?,
        provenance: row.get(11)?,
        trust: row.get(12)?,
        scope: row.get(13)?,
        scope_key: row.get(14)?,
        source_hash: row.get(15)?,
        environment_hash: row.get(16)?,
    })
}

fn relevance_score(record: &ProgramIndexRecord, query: &str, tokens: &[&str]) -> usize {
    if query.is_empty() {
        return 1;
    }
    let name = record.name.to_lowercase();
    let documentation = record.documentation.to_lowercase();
    let source = record.source.to_lowercase();
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

    fn record(name: &str, language: &str, source: &str) -> ProgramIndexRecord {
        ProgramIndexRecord {
            id: uuid::Uuid::new_v4().to_string(),
            version: 0,
            name: name.to_string(),
            language: language.to_string(),
            source: source.to_string(),
            documentation: String::new(),
            signature: None,
            effect: "unclassified".to_string(),
            capabilities_json: "[]".to_string(),
            dependencies_json: "[]".to_string(),
            tests_json: "[]".to_string(),
            provenance: "test".to_string(),
            trust: "candidate".to_string(),
            scope: "session".to_string(),
            scope_key: None,
            source_hash: source.to_string(),
            environment_hash: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn test_program_index_survives_reopen() {
        let temp = TempDir::new().unwrap();
        let reference = {
            let memory = memory(&temp);
            memory
                .index_program_record(record("double", "forth", ": double 2 * ;"))
                .await
                .unwrap()
        };
        let reopened = memory(&temp);
        let stored = reopened
            .get_program_index(&reference.id, reference.version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.name, "double");
        assert_eq!(stored.source, ": double 2 * ;");
    }

    #[tokio::test]
    async fn test_program_index_revision_increments_generation_and_version() {
        let temp = TempDir::new().unwrap();
        let memory = memory(&temp);
        let first = memory
            .index_program_record(record("double", "forth", ": double 2 * ;"))
            .await
            .unwrap();
        let second = memory
            .index_program_record(record("double", "forth", ": double dup + ;"))
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.version, first.version + 1);
        assert_eq!(memory.program_registry_generation().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_save_lisp_define_persists_without_projecting() {
        let temp = TempDir::new().unwrap();
        let memory = memory(&temp);
        memory
            .save_lisp_define("(define (triple x) (* x 3))")
            .await
            .unwrap();
        let defines = memory.load_lisp_defines().await.unwrap();
        assert_eq!(defines, vec!["(define (triple x) (* x 3))".to_string()]);
        assert!(
            memory
                .get_program_index_by_name("triple", Some("lisp"))
                .await
                .unwrap()
                .is_none(),
            "memory must not project Lisp defines into the program index"
        );
    }
}
