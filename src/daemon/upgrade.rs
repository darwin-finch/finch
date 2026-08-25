//! Safe staging and shadow-health preflight for a future daemon supervisor.
//!
//! This module intentionally stops before production socket handoff. Frontend
//! `restart_session` is already safe for frontend re-exec, but daemon takeover
//! requires a detached, crash-recoverable supervisor. The primitive here gives
//! that supervisor immutable candidate and rollback artifacts plus a live,
//! full-daemon candidate proven against a fail-closed snapshot of Brain state.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonUpgradePlan {
    pub candidate: PathBuf,
    pub candidate_sha256: String,
    pub rollback: PathBuf,
    pub rollback_sha256: String,
    pub schema_impact: String,
}

/// A live candidate daemon proven in an isolated namespace.
///
/// Dropping this value terminates the shadow daemon. It confers no authority to
/// stop the incumbent; a future detached supervisor must consume this evidence,
/// persist its own handoff phase, and own promotion/rollback to completion.
pub struct VerifiedDaemonUpgrade {
    plan: DaemonUpgradePlan,
    shadow: Child,
    _shadow_home: tempfile::TempDir,
}

impl Drop for VerifiedDaemonUpgrade {
    fn drop(&mut self) {
        let _ = self.shadow.kill();
        let _ = self.shadow.wait();
    }
}

impl VerifiedDaemonUpgrade {
    pub fn plan(&self) -> &DaemonUpgradePlan {
        &self.plan
    }
}

impl DaemonUpgradePlan {
    /// Hash, execute-preflight, and stage explicit candidate and incumbent
    /// binaries in content-addressed locations before any process handoff.
    pub fn prepare(candidate: &Path, incumbent: &Path, schema_impact: &str) -> Result<Self> {
        let stage_root = dirs::home_dir()
            .context("cannot determine Finch home")?
            .join(".finch/daemon-binaries");
        Self::prepare_with_stage_root(candidate, incumbent, schema_impact, &stage_root)
    }

    /// Variant for an embedder-owned content-addressed artifact store.
    pub fn prepare_with_stage_root(
        candidate: &Path,
        incumbent: &Path,
        schema_impact: &str,
        stage_root: &Path,
    ) -> Result<Self> {
        anyhow::ensure!(
            !schema_impact.trim().is_empty(),
            "schema impact must be recorded, even when it is 'none'"
        );
        let candidate = checked_binary(candidate)?;
        let incumbent = checked_binary(incumbent)?;
        preflight_version(&candidate)?;
        preflight_version(&incumbent)?;
        let candidate_sha256 = hash_file(&candidate)?;
        let rollback_sha256 = hash_file(&incumbent)?;
        anyhow::ensure!(
            candidate_sha256 != rollback_sha256,
            "candidate is identical to the incumbent"
        );
        let candidate = stage_binary(&candidate, &candidate_sha256, stage_root)?;
        let rollback = stage_binary(&incumbent, &rollback_sha256, stage_root)?;
        Ok(Self {
            candidate,
            candidate_sha256,
            rollback,
            rollback_sha256,
            schema_impact: schema_impact.trim().to_string(),
        })
    }

    /// Boot the staged candidate as a complete daemon on isolated HTTP and IPC
    /// endpoints. The production daemon, PID file, socket, and Brain store are
    /// never opened for writing by the candidate.
    pub async fn preflight(self) -> Result<VerifiedDaemonUpgrade> {
        let source = dirs::home_dir().map(|home| home.join(".finch/brains"));
        self.preflight_against(source.as_deref()).await
    }

    /// Preflight against an explicit Brain root, primarily for embedders and
    /// hermetic conformance tests. `None` proves an empty store.
    pub async fn preflight_against(
        self,
        brain_root: Option<&Path>,
    ) -> Result<VerifiedDaemonUpgrade> {
        self.verify_staged()?;
        let shadow_home = isolated_brain_home_from(brain_root)?;
        let bind = unused_loopback_address()?;
        let log_path = shadow_home.path().join("probe.log");
        let log = std::fs::File::create(&log_path)?;
        let mut shadow = Command::new(&self.candidate)
            .arg("daemon")
            .arg("--bind")
            .arg(bind.to_string())
            .env("HOME", shadow_home.path())
            .env("USERPROFILE", shadow_home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .with_context(|| format!("start shadow daemon {}", self.candidate.display()))?;
        let socket = shadow_home.path().join(".finch/daemon.sock");
        if let Err(error) = wait_for_shadow_health(bind, socket).await {
            let status = shadow.try_wait().ok().flatten();
            let _ = shadow.kill();
            let _ = shadow.wait();
            let detail = std::fs::read_to_string(log_path).unwrap_or_default();
            return Err(error.context(format!(
                "shadow candidate exited with {status:?}: {}",
                detail.trim()
            )));
        }
        Ok(VerifiedDaemonUpgrade {
            plan: self,
            shadow,
            _shadow_home: shadow_home,
        })
    }

    fn verify_staged(&self) -> Result<()> {
        anyhow::ensure!(
            hash_file(&self.candidate)? == self.candidate_sha256,
            "staged candidate digest changed"
        );
        anyhow::ensure!(
            hash_file(&self.rollback)? == self.rollback_sha256,
            "staged rollback digest changed"
        );
        Ok(())
    }
}

async fn wait_for_shadow_health(bind: SocketAddr, socket: PathBuf) -> Result<()> {
    let base_url = format!("http://{bind}");
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if super::spawn::health_check_succeeds(&base_url).await {
            let verifier = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    let client = crate::ipc::IpcClient::connect_path(socket).await?;
                    verify_fresh_brain_bootstrap(&client).await?;
                    drop(client);
                    Ok::<_, anyhow::Error>(())
                })
            });
            return tokio::time::timeout(Duration::from_secs(5), verifier)
                .await
                .context("shadow IPC protocol handshake timed out")?
                .context("shadow IPC verifier task failed")?;
        }
    }
    anyhow::bail!("shadow daemon did not answer health checks within 15 seconds")
}

/// Exercise the two reverse capabilities which historically closed during a
/// fresh-home startup even though a protocol ping succeeded.
async fn verify_fresh_brain_bootstrap(client: &crate::ipc::IpcClient) -> Result<()> {
    let suffix = uuid::Uuid::new_v4();
    let brain = format!("preflight-{}", suffix.simple());
    let subject = format!("preflight/frontend-{suffix}");
    let snapshot = client.brain_snapshot(&brain).await?;
    client.brain_claim_runner_identity(&subject).await?;
    let lease = client
        .brain_acquire_runner(
            &brain,
            &subject,
            &snapshot.environment,
            None,
            30_000,
        )
        .await?;
    let (runner_tx, _runner_rx) = tokio::sync::mpsc::unbounded_channel();
    let _runner = client
        .register_brain_runner(&brain, lease.lease_id, runner_tx)
        .await
        .context("fresh daemon rejected runner callback")?;
    let attachment = client
        .brain_attach(
            &brain,
            &format!("preflight-driver-{suffix}"),
            crate::brain::store::AttachmentRole::Driver,
            None,
        )
        .await?;
    let mut watch = client.brain_watch(&brain, &attachment).await?;
    let first = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .context("fresh daemon Brain watch timed out")?
        .context("fresh daemon Brain watch closed")??;
    let crate::brain::store::BrainWireMessage::Snapshot { brain: watched } = first else {
        anyhow::bail!("fresh daemon Brain watch did not begin with a snapshot");
    };
    anyhow::ensure!(
        watched
            .runner_lease
            .as_ref()
            .is_some_and(|runner| runner.lease_id == lease.lease_id),
        "fresh daemon snapshot lost its registered runner lease"
    );
    client.brain_detach(&brain, &attachment).await?;
    client.brain_release_runner(&brain, lease.lease_id).await?;
    Ok(())
}

fn isolated_brain_home_from(brain_root: Option<&Path>) -> Result<tempfile::TempDir> {
    let home = tempfile::Builder::new()
        .prefix("finch-daemon-probe-")
        .tempdir()?;
    let finch = home.path().join(".finch");
    std::fs::create_dir_all(&finch)?;
    // No production credentials, memory database, model cache, PID, or socket
    // enters the probe namespace.
    std::fs::write(
        finch.join("config.toml"),
        "[[providers]]\ntype = \"openai\"\napi_key = \"sk-daemon-upgrade-probe\"\n\n[server]\nadvertise = false\nauth_enabled = false\n",
    )?;
    if let Some(source) = brain_root {
        if source.exists() {
            snapshot_tree(source, &finch.join("brains"))?;
        }
    }
    Ok(home)
}

/// Copy only a stable source tree. A concurrent append or checkpoint rename
/// changes the manifest and makes preflight fail closed rather than testing a
/// torn view and calling it healthy.
fn snapshot_tree(source: &Path, destination: &Path) -> Result<()> {
    let before = tree_manifest(source)?;
    copy_tree(source, destination)?;
    let after = tree_manifest(source)?;
    let copied = tree_manifest(destination)?;
    anyhow::ensure!(
        before == after && after == copied,
        "Brain store changed during daemon preflight snapshot; retry after the current turn commits"
    );
    Ok(())
}

fn tree_manifest(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, path) in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        let mut file = std::fs::File::open(path)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update([0xff]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if entry.file_type()?.is_file() {
            files.push((entry.path().strip_prefix(root)?.to_path_buf(), entry.path()));
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn unused_loopback_address() -> Result<SocketAddr> {
    let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

fn checked_binary(path: &Path) -> Result<PathBuf> {
    let path = std::fs::canonicalize(path)
        .with_context(|| format!("binary does not exist: {}", path.display()))?;
    let metadata = std::fs::metadata(&path)?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a regular file",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "{} is not executable",
            path.display()
        );
    }
    Ok(path)
}

fn preflight_version(path: &Path) -> Result<()> {
    let output = Command::new(path).arg("--version").output()?;
    anyhow::ensure!(
        output.status.success(),
        "{} failed --version: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn stage_binary(source: &Path, sha256: &str, stage_root: &Path) -> Result<PathBuf> {
    let root = stage_root.join(sha256);
    std::fs::create_dir_all(&root)?;
    let destination = root.join(if cfg!(windows) { "finch.exe" } else { "finch" });
    if !destination.exists() || hash_file(&destination)? != sha256 {
        let temporary = root.join("finch.tmp");
        std::fs::copy(source, &temporary)?;
        std::fs::rename(temporary, &destination)?;
    }
    anyhow::ensure!(
        hash_file(&destination)? == sha256,
        "staged binary digest mismatch"
    );
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_probe_contains_only_snapshot_and_minimal_config() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("example")).unwrap();
        std::fs::write(source.path().join("example/events.jsonl"), "event\n").unwrap();
        let home = isolated_brain_home_from(Some(source.path())).unwrap();
        let mut entries = std::fs::read_dir(home.path().join(".finch"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, ["brains", "config.toml"]);
        assert_eq!(
            std::fs::read_to_string(home.path().join(".finch/brains/example/events.jsonl"))
                .unwrap(),
            "event\n"
        );
    }

    #[test]
    fn snapshot_manifest_detects_content_changes() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("events.jsonl"), "one\n").unwrap();
        let first = tree_manifest(source.path()).unwrap();
        std::fs::write(source.path().join("events.jsonl"), "one\ntwo\n").unwrap();
        let second = tree_manifest(source.path()).unwrap();
        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn prepare_stages_distinct_immutable_recovery_artifacts() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let candidate = temp.path().join("candidate");
        let incumbent = temp.path().join("incumbent");
        std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&incumbent, "#!/bin/sh\n# old\nexit 0\n").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&incumbent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan = DaemonUpgradePlan::prepare_with_stage_root(
            &candidate,
            &incumbent,
            "none",
            &temp.path().join("stage"),
        )
        .unwrap();
        assert_ne!(plan.candidate_sha256, plan.rollback_sha256);
        assert_eq!(hash_file(&plan.candidate).unwrap(), plan.candidate_sha256);
        assert_eq!(hash_file(&plan.rollback).unwrap(), plan.rollback_sha256);
    }
}
