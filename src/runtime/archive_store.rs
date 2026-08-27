//! Atomic application-owned persistence for reducible typed VM state.

use crate::runtime::{ProgramRuntime, ProgramRuntimeArchive, ProgramRuntimeAuthorityState};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const RUNTIME_ARCHIVE_FILE_VERSION: u32 = 1;
const RUNTIME_AUTHORITY_FILE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct StoredRuntimeArchive {
    format_version: u32,
    archive_sha256: String,
    archive: ProgramRuntimeArchive,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRuntimeAuthority {
    format_version: u32,
    authority_sha256: String,
    authority: ProgramRuntimeAuthorityState,
}

/// Version-1 authority payload before capability policy became explicit.
/// Field order is part of the historical SHA-256 input and therefore must not
/// be changed. This exists only to verify and migrate an already-written file.
#[derive(Serialize)]
struct LegacyRuntimeAuthority<'a> {
    format_version: u32,
    session_id: uuid::Uuid,
    project_id: &'a str,
    ledger: &'a crate::vm::CapabilityLedger,
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

/// Durable storage for application-owned authority associated with one
/// [`ProgramRuntime`]. This record is deliberately separate from the VM
/// archive: copying or restoring executable state must never confer grants.
#[derive(Debug, Clone)]
pub struct ProgramRuntimeAuthorityStore {
    path: PathBuf,
}

impl ProgramRuntimeAuthorityStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Validate and atomically replace the stored authority record.
    pub fn save(&self, runtime: &ProgramRuntime) -> Result<()> {
        self.save_state(runtime.authority_state()?)
    }

    /// Validate and atomically replace an application-supplied authority
    /// snapshot. This is the sink used by named Brain policy mutations; it
    /// never reads reducible VM state or a VM checkpoint.
    pub fn save_state(&self, authority: ProgramRuntimeAuthorityState) -> Result<()> {
        validate_authority_state(authority.clone())?;
        let authority_bytes = serde_json::to_vec(&authority)?;
        let stored = StoredRuntimeAuthority {
            format_version: RUNTIME_AUTHORITY_FILE_VERSION,
            authority_sha256: hex::encode(Sha256::digest(&authority_bytes)),
            authority,
        };
        atomic_write_json(&self.path, &stored, "runtime authority")
    }

    pub fn load_state(&self) -> Result<Option<ProgramRuntimeAuthorityState>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read runtime authority '{}'", self.path.display()))
            }
        };
        let raw: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("decode runtime authority '{}'", self.path.display()))?;
        let policy_was_present = raw
            .get("authority")
            .and_then(|authority| authority.get("policy"))
            .is_some();
        let stored: StoredRuntimeAuthority = serde_json::from_value(raw)
            .with_context(|| format!("decode runtime authority '{}'", self.path.display()))?;
        if stored.format_version != RUNTIME_AUTHORITY_FILE_VERSION {
            bail!(
                "runtime authority file '{}' has version {}; expected {}",
                self.path.display(),
                stored.format_version,
                RUNTIME_AUTHORITY_FILE_VERSION
            );
        }
        let authority_bytes = serde_json::to_vec(&stored.authority)?;
        let actual = hex::encode(Sha256::digest(&authority_bytes));
        let legacy_actual = (!policy_was_present)
            .then(|| {
                serde_json::to_vec(&LegacyRuntimeAuthority {
                    format_version: stored.authority.format_version,
                    session_id: stored.authority.session_id,
                    project_id: &stored.authority.project_id,
                    ledger: &stored.authority.ledger,
                })
            })
            .transpose()?
            .map(|bytes| hex::encode(Sha256::digest(&bytes)));
        if actual != stored.authority_sha256
            && legacy_actual.as_deref() != Some(stored.authority_sha256.as_str())
        {
            bail!(
                "runtime authority file '{}' failed its SHA-256 integrity check",
                self.path.display()
            );
        }
        validate_authority_state(stored.authority.clone())?;
        Ok(Some(stored.authority))
    }

    /// Restore a stored authority record into a newly constructed runtime.
    /// A missing record leaves the runtime authority-free.
    pub fn restore_into(&self, runtime: &mut ProgramRuntime) -> Result<bool> {
        let Some(authority) = self.load_state()? else {
            return Ok(false);
        };
        runtime.restore_authority_state(authority)?;
        Ok(true)
    }
}

fn validate_authority_state(authority: ProgramRuntimeAuthorityState) -> Result<()> {
    let mut runtime = ProgramRuntime::new();
    runtime.restore_authority_state(authority)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{label} path '{}' has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create {label} directory '{}'", parent.display()))?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary {label} in '{}'", parent.display()))?;
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
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {label} '{}'", path.display()))?;
    sync_parent_directory(parent)
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
    use crate::vm::{CapabilityKind, CapabilityRequirement, GrantScope, ResourceSelector};

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
                capability: CapabilityKind::AgentSpawn,
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

    #[test]
    fn authority_store_round_trips_scoped_grants_and_audit() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeAuthorityStore::new(directory.path().join("authority.json"));
        let runtime = ProgramRuntime::new();
        runtime
            .issue_typed_capability(
                CapabilityRequirement {
                    capability: CapabilityKind::AgentSpawn,
                    selector: ResourceSelector::None,
                },
                GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        store.save(&runtime).unwrap();

        let state = store.load_state().unwrap().expect("stored authority");
        assert_eq!(state.session_id, runtime.capability_session_id());
        assert_eq!(state.project_id, runtime.capability_project_id());
        assert_eq!(state.ledger.grants.grants.len(), 1);
        assert_eq!(state.ledger.audit.len(), 1);

        let mut restored = ProgramRuntime::new();
        assert!(store.restore_into(&mut restored).unwrap());
        assert_eq!(
            restored.capability_session_id(),
            runtime.capability_session_id()
        );
        assert_eq!(restored.capability_ledger().unwrap(), state.ledger);
    }

    #[test]
    fn authority_store_migrates_the_signed_pre_policy_payload() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeAuthorityStore::new(directory.path().join("authority.json"));
        let runtime = ProgramRuntime::new();
        runtime
            .issue_typed_capability(
                CapabilityRequirement {
                    capability: CapabilityKind::AgentSpawn,
                    selector: ResourceSelector::None,
                },
                GrantScope::Global,
                "test-user",
                None,
            )
            .unwrap();
        let state = runtime.authority_state().unwrap();
        let legacy = LegacyRuntimeAuthority {
            format_version: state.format_version,
            session_id: state.session_id,
            project_id: &state.project_id,
            ledger: &state.ledger,
        };
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        let stored = serde_json::json!({
            "format_version": RUNTIME_AUTHORITY_FILE_VERSION,
            "authority_sha256": hex::encode(Sha256::digest(&legacy_bytes)),
            "authority": serde_json::from_slice::<serde_json::Value>(&legacy_bytes).unwrap(),
        });
        std::fs::write(store.path(), serde_json::to_vec(&stored).unwrap()).unwrap();

        let restored = store.load_state().unwrap().expect("legacy authority");
        assert_eq!(restored.policy.policy_hash, "finch-local-runtime-v1");
        assert!(restored.policy.denied_capabilities.is_empty());
        assert_eq!(restored.ledger, state.ledger);
    }

    #[test]
    fn authority_store_detects_tampering_before_restore() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeAuthorityStore::new(directory.path().join("authority.json"));
        store.save(&ProgramRuntime::new()).unwrap();

        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        value["authority"]["project_id"] = serde_json::json!("different-project");
        std::fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let error = store
            .load_state()
            .err()
            .expect("authority tampering must fail closed");
        assert!(error.to_string().contains("integrity check"));
    }

    #[test]
    fn authority_store_rejects_an_active_obsolete_policy() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeAuthorityStore::new(directory.path().join("authority.json"));
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(CapabilityRequirement {
                capability: CapabilityKind::AgentSpawn,
                selector: ResourceSelector::None,
            })
            .unwrap();
        let mut state = runtime.authority_state().unwrap();
        state.ledger.grants.grants[0].policy_hash = "obsolete-policy".into();

        let error = store
            .save_state(state)
            .err()
            .expect("obsolete authority must not be persisted");
        assert!(error.to_string().contains("another policy"));
        assert!(!store.path().exists());
    }

    #[test]
    fn missing_authority_store_confers_no_grants() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProgramRuntimeAuthorityStore::new(directory.path().join("missing.json"));
        let mut runtime = ProgramRuntime::new();
        assert!(!store.restore_into(&mut runtime).unwrap());
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
    }
}
