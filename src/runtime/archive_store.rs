//! Atomic application-owned persistence for reducible typed VM state.

use crate::runtime::{ProgramRuntime, ProgramRuntimeArchive};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const RUNTIME_ARCHIVE_FILE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct StoredRuntimeArchive {
    format_version: u32,
    archive_sha256: String,
    archive: ProgramRuntimeArchive,
}

/// Durable storage for one persistent [`ProgramRuntime`].
///
/// The archive contains only reducible VM revision state. Capability grants,
/// pending host calls, delivery acknowledgements, and live host resources
/// have separate application-owned lifecycles and are never smuggled into
/// this file.
#[derive(Debug, Clone)]
pub struct ProgramRuntimeArchiveStore {
    path: PathBuf,
}

impl ProgramRuntimeArchiveStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Validate and atomically replace the stored runtime archive.
    pub fn save(&self, runtime: &ProgramRuntime) -> Result<()> {
        self.save_archive(runtime.archive()?)
    }

    fn save_archive(&self, archive: ProgramRuntimeArchive) -> Result<()> {
        // Validate the complete lineage before replacing a known-good file.
        // This also proves the current checkpoint can be reconstructed using
        // the current verifier and core vocabulary.
        ProgramRuntime::from_archive(archive.clone())?;

        let archive_bytes = serde_json::to_vec(&archive)?;
        let stored = StoredRuntimeArchive {
            format_version: RUNTIME_ARCHIVE_FILE_VERSION,
            archive_sha256: hex::encode(Sha256::digest(&archive_bytes)),
            archive,
        };
        let bytes = serde_json::to_vec(&stored)?;
        let parent = self.path.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "runtime archive path '{}' has no parent",
                self.path.display()
            )
        })?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create runtime archive directory '{}'", parent.display()))?;

        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!("create temporary runtime archive in '{}'", parent.display())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        temporary.write_all(&bytes)?;
        temporary.write_all(b"\n")?;
        temporary.as_file_mut().sync_all()?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace runtime archive '{}'", self.path.display()))?;
        sync_parent_directory(parent)?;
        Ok(())
    }

    pub fn load_archive(&self) -> Result<Option<ProgramRuntimeArchive>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read runtime archive '{}'", self.path.display()))
            }
        };
        let stored: StoredRuntimeArchive = serde_json::from_slice(&bytes)
            .with_context(|| format!("decode runtime archive '{}'", self.path.display()))?;
        if stored.format_version != RUNTIME_ARCHIVE_FILE_VERSION {
            bail!(
                "runtime archive file '{}' has version {}; expected {}",
                self.path.display(),
                stored.format_version,
                RUNTIME_ARCHIVE_FILE_VERSION
            );
        }
        let archive_bytes = serde_json::to_vec(&stored.archive)?;
        let actual = hex::encode(Sha256::digest(&archive_bytes));
        if actual != stored.archive_sha256 {
            bail!(
                "runtime archive file '{}' failed its SHA-256 integrity check",
                self.path.display()
            );
        }
        // A checksum proves only byte integrity. Re-run the archive's semantic
        // validation before exposing it to an application.
        ProgramRuntime::from_archive(stored.archive.clone())?;
        Ok(Some(stored.archive))
    }

    pub fn load(&self) -> Result<Option<ProgramRuntime>> {
        self.load_archive()?
            .map(ProgramRuntime::from_archive)
            .transpose()
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs::{ExecutionEffect, ProgramLanguage};
    use crate::runtime::{ProgramSubmission, ProgramValue};
    use crate::vm::{CapabilityKind, CapabilityRequirement, ResourceSelector};

    fn submission(runtime: &ProgramRuntime, source: &str) -> ProgramSubmission {
        ProgramSubmission {
            language: ProgramLanguage::Lisp,
            source_id: Some("archive-store-test.lisp".into()),
            source: source.into(),
            intent: "test durable runtime archive storage".into(),
            effect: ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        }
    }

    #[tokio::test]
    async fn atomically_restores_reducible_revision_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeArchiveStore::new(directory.path().join("runtime.json"));
        let runtime = ProgramRuntime::new();
        runtime
            .submit_typed_only(submission(
                &runtime,
                "(define (double (n : int)) : int (* n 2))",
            ))
            .await
            .unwrap();
        runtime
            .submit_typed_only(submission(&runtime, "(double 21)"))
            .await
            .unwrap();
        store.save(&runtime).unwrap();

        let restored = store.load().unwrap().expect("stored runtime");
        assert_eq!(restored.revision(), 2);
        assert_eq!(restored.revision_history().unwrap().len(), 3);
        let outcome = restored
            .submit_typed_only(submission(&restored, "(double 50)"))
            .await
            .unwrap();
        assert_eq!(
            outcome.values,
            vec![ProgramValue::Int(42), ProgramValue::Int(100)]
        );
    }

    #[tokio::test]
    async fn restored_runtime_does_not_inherit_authority() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeArchiveStore::new(directory.path().join("runtime.json"));
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: ResourceSelector::None,
            })
            .unwrap();
        runtime
            .submit_typed_only(submission(&runtime, "(+ 1 2)"))
            .await
            .unwrap();
        store.save(&runtime).unwrap();

        let restored = store.load().unwrap().expect("stored runtime");
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
        assert!(restored
            .inspect()
            .await
            .unwrap()
            .granted_capabilities
            .iter()
            .all(|requirement| requirement.capability != CapabilityKind::ProcessRun));
    }

    #[tokio::test]
    async fn detects_archive_tampering_before_restore() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeArchiveStore::new(directory.path().join("runtime.json"));
        let runtime = ProgramRuntime::new();
        store.save(&runtime).unwrap();

        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        value["archive"]["current_revision"] = serde_json::json!(99);
        std::fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let error = store.load().err().expect("tampering must fail closed");
        assert!(error.to_string().contains("integrity check"));
    }

    #[test]
    fn missing_archive_is_not_an_empty_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeArchiveStore::new(directory.path().join("missing.json"));
        assert!(store.load().unwrap().is_none());
    }
}
