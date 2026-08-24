//! Opt-in, source-bearing provider corpus and non-executing replay.
//!
//! Aggregate wire metrics deliberately contain no source. Real model output is
//! useful for language evolution, however, so Finch can separately retain it
//! when `FINCH_WIRE_CORPUS_PATH` names a JSONL file. Replay only invokes the
//! readers, frontends, and verifier; it never starts a [`ProgramRuntime`].

use super::{classify_wire_failure, wire_diagnostic_code, ProgramLanguage};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::vm::frontend::{compile_forth, compile_lisp};

pub const WIRE_CORPUS_FORMAT_VERSION: u32 = 1;
pub const WIRE_CORPUS_PATH_ENV: &str = "FINCH_WIRE_CORPUS_PATH";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCorpusAttempt {
    FirstPass,
    Repair,
}

/// One raw text response exactly as received from a provider.
///
/// This record is intentionally source-bearing and therefore separate from
/// normal metrics. It contains no user prompt, tool result, credential, or VM
/// output. Users still control whether and where it is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCorpusEntry {
    pub format_version: u32,
    pub manifest_protocol_version: u32,
    pub vm_type_system_version: u32,
    pub captured_at: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub attempt: WireCorpusAttempt,
    pub source_sha256: String,
    pub source: String,
}

impl WireCorpusEntry {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        surface: impl Into<String>,
        attempt: WireCorpusAttempt,
        source: impl Into<String>,
    ) -> Self {
        let source = source.into();
        Self {
            format_version: WIRE_CORPUS_FORMAT_VERSION,
            manifest_protocol_version: super::MANIFEST_PROTOCOL_VERSION,
            vm_type_system_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            captured_at: Utc::now(),
            provider: provider.into(),
            model: model.into(),
            surface: surface.into(),
            attempt,
            source_sha256: hex::encode(Sha256::digest(source.as_bytes())),
            source,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WireCorpusLogger {
    path: PathBuf,
}

impl WireCorpusLogger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_env() -> Option<Self> {
        std::env::var_os(WIRE_CORPUS_PATH_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .map(Self::new)
    }

    pub fn append(&self, entry: &WireCorpusEntry) -> Result<()> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create wire corpus directory {}", parent.display()))?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.path)
            .with_context(|| format!("open wire corpus {}", self.path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("lock wire corpus {}", self.path.display()))?;
        let encoded = serde_json::to_vec(entry).context("encode wire corpus entry")?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.unlock()?;
        Ok(())
    }
}

pub fn capture_from_env(
    provider: &str,
    model: &str,
    surface: &str,
    attempt: WireCorpusAttempt,
    source: &str,
) {
    let Some(logger) = WireCorpusLogger::from_env() else {
        return;
    };
    let entry = WireCorpusEntry::new(provider, model, surface, attempt, source);
    if let Err(error) = logger.append(&entry) {
        tracing::warn!("failed to append opt-in provider wire corpus: {error}");
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCorpusCounts {
    pub total: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub forth: usize,
    pub lisp: usize,
    pub diagnostics: BTreeMap<String, usize>,
    pub failure_classes: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCorpusAudit {
    pub format_version: u32,
    pub counts: WireCorpusCounts,
    pub by_provider_model: BTreeMap<String, WireCorpusCounts>,
    pub source_versions: BTreeMap<String, usize>,
}

impl WireCorpusCounts {
    fn record_language(&mut self, language: ProgramLanguage) {
        match language {
            ProgramLanguage::Forth => self.forth += 1,
            ProgramLanguage::Lisp => self.lisp += 1,
        }
    }

    fn accept(&mut self) {
        self.total += 1;
        self.accepted += 1;
    }

    fn reject(&mut self, source: &str, diagnostic: &str) {
        self.total += 1;
        self.rejected += 1;
        let code = wire_diagnostic_code(diagnostic).unwrap_or_else(|| "unknown".to_string());
        *self.diagnostics.entry(code).or_default() += 1;
        let class = format!("{:?}", classify_wire_failure(source, diagnostic));
        *self.failure_classes.entry(class).or_default() += 1;
    }
}

/// Compile and verify every retained source without interpreting any module.
pub fn audit(path: &Path) -> Result<WireCorpusAudit> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("open wire corpus {}", path.display()))?;
    let vocabulary = crate::vm::core_vocabulary();
    let mut audit = WireCorpusAudit {
        format_version: WIRE_CORPUS_FORMAT_VERSION,
        ..WireCorpusAudit::default()
    };

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read corpus line {}", line_index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: WireCorpusEntry = serde_json::from_str(&line)
            .with_context(|| format!("decode corpus line {}", line_index + 1))?;
        if entry.format_version != WIRE_CORPUS_FORMAT_VERSION {
            bail!(
                "corpus line {} has format version {}; expected {}",
                line_index + 1,
                entry.format_version,
                WIRE_CORPUS_FORMAT_VERSION
            );
        }
        let actual_hash = hex::encode(Sha256::digest(entry.source.as_bytes()));
        if actual_hash != entry.source_sha256 {
            bail!(
                "corpus line {} failed its source hash check",
                line_index + 1
            );
        }

        let source_version = format!(
            "manifest-{}/vm-{}",
            entry.manifest_protocol_version, entry.vm_type_system_version
        );
        *audit.source_versions.entry(source_version).or_default() += 1;

        let key = format!("{}/{}", entry.provider, entry.model);
        let language = ProgramLanguage::infer_wire_source(&entry.source);
        if let Ok(language) = language {
            audit.counts.record_language(language);
            audit
                .by_provider_model
                .entry(key.clone())
                .or_default()
                .record_language(language);
        }
        let result: std::result::Result<(), String> = language
            .map_err(|error| error.to_string())
            .and_then(|language| {
                let verified = match language {
                    ProgramLanguage::Forth => {
                        compile_forth("wire-corpus.forth", &entry.source, Vec::new(), &vocabulary)
                    }
                    ProgramLanguage::Lisp => {
                        compile_lisp("wire-corpus.lisp", &entry.source, Vec::new(), &vocabulary)
                    }
                };
                verified.map(|_| ()).map_err(|diagnostics| {
                    diagnostics
                        .first()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "E-VERIFY-000: verifier rejected source".to_string())
                })
            });
        match result {
            Ok(()) => {
                audit.counts.accept();
                audit.by_provider_model.entry(key).or_default().accept();
            }
            Err(error) => {
                let diagnostic = error.to_string();
                audit.counts.reject(&entry.source, &diagnostic);
                audit
                    .by_provider_model
                    .entry(key)
                    .or_default()
                    .reject(&entry.source, &diagnostic);
            }
        }
    }
    Ok(audit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_round_trip_and_report_only_audit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wire.jsonl");
        let logger = WireCorpusLogger::new(&path);
        logger
            .append(&WireCorpusEntry::new(
                "xai",
                "grok",
                "interactive",
                WireCorpusAttempt::FirstPass,
                "(say \"hello\")",
            ))
            .unwrap();
        logger
            .append(&WireCorpusEntry::new(
                "xai",
                "grok",
                "interactive",
                WireCorpusAttempt::Repair,
                "Hello there!",
            ))
            .unwrap();

        let report = audit(&path).unwrap();
        assert_eq!(report.counts.total, 2);
        assert_eq!(report.counts.accepted, 1);
        assert_eq!(report.counts.rejected, 1);
        assert_eq!(report.counts.lisp, 1);
        assert_eq!(report.counts.forth, 1);
        assert_eq!(report.counts.failure_classes.get("RawProse"), Some(&1));
        assert_eq!(report.by_provider_model["xai/grok"].total, 2);
    }

    #[test]
    fn corpus_detects_source_tampering() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wire.jsonl");
        let mut entry = WireCorpusEntry::new(
            "xai",
            "grok",
            "one_shot",
            WireCorpusAttempt::FirstPass,
            "(say \"hello\")",
        );
        entry.source = "(say \"changed\")".to_string();
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        assert!(audit(&path).unwrap_err().to_string().contains("hash check"));
    }
}
