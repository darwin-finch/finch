//! Safe staging, shadow preflight, and crash-recoverable daemon handoff.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SUPERVISOR_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonUpgradePlan {
    pub candidate: PathBuf,
    pub candidate_sha256: String,
    pub rollback: PathBuf,
    pub rollback_sha256: String,
    pub schema_impact: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductionDaemonEndpoints {
    pub bind: SocketAddr,
    pub pid_file: PathBuf,
    pub ipc_socket: PathBuf,
    /// HOME/USERPROFILE passed to both candidate and rollback daemons.
    pub home: PathBuf,
}

impl ProductionDaemonEndpoints {
    pub fn for_home(home: &Path, bind: SocketAddr) -> Self {
        Self {
            bind,
            pid_file: home.join(".finch/daemon.pid"),
            ipc_socket: home.join(".finch/daemon.sock"),
            home: home.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorPhase {
    IntentRecorded,
    IncumbentStopping,
    CandidateStarting,
    CandidateVerifying,
    RollbackStarting,
    RollbackVerifying,
    Promoted,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonUpgradeRecord {
    pub version: u32,
    pub phase: SupervisorPhase,
    pub candidate: PathBuf,
    pub candidate_sha256: String,
    pub rollback: PathBuf,
    pub rollback_sha256: String,
    pub schema_impact: String,
    pub endpoints: ProductionDaemonEndpoints,
    pub incumbent_pid: u32,
    pub candidate_pid: Option<u32>,
    pub rollback_pid: Option<u32>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorLaunch {
    pub supervisor_pid: u32,
    pub record_path: PathBuf,
}

/// A live candidate daemon proven in an isolated namespace.
///
/// Dropping this value terminates the shadow daemon. Production mutation only
/// begins after [`VerifiedDaemonUpgrade::launch_supervisor`] durably records
/// intent and starts the detached internal supervisor.
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

    /// Persist the complete handoff intent and launch an internal detached
    /// supervisor. The returned PID is the supervisor, not the candidate, and
    /// is deliberately not a success claim for the upgrade.
    pub fn launch_supervisor(
        self,
        incumbent_pid: u32,
        endpoints: ProductionDaemonEndpoints,
        record_path: &Path,
    ) -> Result<SupervisorLaunch> {
        anyhow::ensure!(incumbent_pid != 0, "incumbent PID must be explicit");
        self.plan.verify_staged()?;
        let record = DaemonUpgradeRecord {
            version: SUPERVISOR_RECORD_VERSION,
            phase: SupervisorPhase::IntentRecorded,
            candidate: self.plan.candidate.clone(),
            candidate_sha256: self.plan.candidate_sha256.clone(),
            rollback: self.plan.rollback.clone(),
            rollback_sha256: self.plan.rollback_sha256.clone(),
            schema_impact: self.plan.schema_impact.clone(),
            endpoints,
            incumbent_pid,
            candidate_pid: None,
            rollback_pid: None,
            failure: None,
        };
        let intent_lock = acquire_record_lock(record_path)?;
        anyhow::ensure!(
            !record_path.exists(),
            "daemon upgrade record already exists: {}",
            record_path.display()
        );
        write_record(record_path, &record)?;
        let launch = spawn_detached_supervisor(record_path)?;
        drop(intent_lock);
        drop(self);
        Ok(launch)
    }
}

/// Resume a persisted upgrade. This is an internal process entrypoint, not a
/// model-facing operation. Re-running it after a supervisor crash deterministically
/// verifies an existing candidate or enters rollback.
pub async fn run_supervisor(record_path: &Path) -> Result<()> {
    let _lock = acquire_record_lock(record_path)?;

    let mut record = read_record(record_path)?;
    validate_record(&record)?;
    if matches!(
        record.phase,
        SupervisorPhase::Promoted | SupervisorPhase::RolledBack
    ) {
        return Ok(());
    }

    let outcome = supervise(&mut record, record_path).await;
    if let Err(error) = &outcome {
        record.failure = Some(format!("{error:#}"));
        let _ = write_record(record_path, &record);
    }
    outcome
}

async fn supervise(record: &mut DaemonUpgradeRecord, record_path: &Path) -> Result<()> {
    match record.phase {
        SupervisorPhase::IntentRecorded | SupervisorPhase::IncumbentStopping => {
            set_phase(record, record_path, SupervisorPhase::IncumbentStopping)?;
            if process_exists(record.incumbent_pid) {
                anyhow::ensure!(
                    process_binary_matches(record.incumbent_pid, &record.rollback_sha256),
                    "refusing to stop incumbent PID {} because its image is not the recorded rollback hash",
                    record.incumbent_pid
                );
                stop_pid_if_binary(record.incumbent_pid, &record.rollback_sha256).await?;
            }
            set_phase(record, record_path, SupervisorPhase::CandidateStarting)?;
        }
        SupervisorPhase::CandidateStarting
        | SupervisorPhase::CandidateVerifying
        | SupervisorPhase::RollbackStarting
        | SupervisorPhase::RollbackVerifying => {}
        SupervisorPhase::Failed => {
            anyhow::bail!("upgrade record is in a non-recoverable failed phase")
        }
        SupervisorPhase::Promoted | SupervisorPhase::RolledBack => return Ok(()),
    }

    if matches!(
        record.phase,
        SupervisorPhase::CandidateStarting | SupervisorPhase::CandidateVerifying
    ) {
        if record.phase == SupervisorPhase::CandidateVerifying {
            if let Some(pid) = record.candidate_pid {
                if wait_for_production_health(pid, &record.candidate_sha256, &record.endpoints)
                    .await
                    .is_ok()
                {
                    return set_phase(record, record_path, SupervisorPhase::Promoted);
                }
                stop_pid_if_binary(pid, &record.candidate_sha256).await?;
            }
        }
        if record.phase == SupervisorPhase::CandidateStarting {
            if let Some(pid) =
                discover_spawned_pid(&record.endpoints, &record.candidate_sha256).await
            {
                record.candidate_pid = Some(pid);
                set_phase(record, record_path, SupervisorPhase::CandidateVerifying)?;
                if wait_for_production_health(pid, &record.candidate_sha256, &record.endpoints)
                    .await
                    .is_ok()
                {
                    return set_phase(record, record_path, SupervisorPhase::Promoted);
                }
                stop_pid_if_binary(pid, &record.candidate_sha256).await?;
                set_phase(record, record_path, SupervisorPhase::RollbackStarting)?;
            } else {
                let pid = spawn_production(&record.candidate, &record.endpoints, "candidate")?;
                record.candidate_pid = Some(pid);
                set_phase(record, record_path, SupervisorPhase::CandidateVerifying)?;
                if wait_for_production_health(pid, &record.candidate_sha256, &record.endpoints)
                    .await
                    .is_ok()
                {
                    return set_phase(record, record_path, SupervisorPhase::Promoted);
                }
                stop_pid_if_binary(pid, &record.candidate_sha256).await?;
            }
        }
        set_phase(record, record_path, SupervisorPhase::RollbackStarting)?;
    }

    if record.phase == SupervisorPhase::RollbackVerifying {
        if let Some(pid) = record.rollback_pid {
            if wait_for_production_health(pid, &record.rollback_sha256, &record.endpoints)
                .await
                .is_ok()
            {
                return set_phase(record, record_path, SupervisorPhase::RolledBack);
            }
            stop_pid_if_binary(pid, &record.rollback_sha256).await?;
        }
        record.phase = SupervisorPhase::RollbackStarting;
    }
    if record.phase == SupervisorPhase::RollbackStarting {
        if let Some(pid) = discover_spawned_pid(&record.endpoints, &record.rollback_sha256).await {
            record.rollback_pid = Some(pid);
            set_phase(record, record_path, SupervisorPhase::RollbackVerifying)?;
            wait_for_production_health(pid, &record.rollback_sha256, &record.endpoints)
                .await
                .context("recovered rollback daemon failed full production health verification")?;
            return set_phase(record, record_path, SupervisorPhase::RolledBack);
        }
        let pid = spawn_production(&record.rollback, &record.endpoints, "rollback")?;
        record.rollback_pid = Some(pid);
        set_phase(record, record_path, SupervisorPhase::RollbackVerifying)?;
        wait_for_production_health(pid, &record.rollback_sha256, &record.endpoints)
            .await
            .context("rollback daemon failed full production health verification")?;
        return set_phase(record, record_path, SupervisorPhase::RolledBack);
    }
    anyhow::bail!("unsupported supervisor phase: {:?}", record.phase)
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

fn read_record(path: &Path) -> Result<DaemonUpgradeRecord> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read daemon upgrade record {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode daemon upgrade record {}", path.display()))
}

fn acquire_record_lock(path: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    let parent = path
        .parent()
        .context("daemon upgrade record must have a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.lock_exclusive()
        .context("lock daemon upgrade supervisor record")?;
    Ok(lock)
}

fn validate_record(record: &DaemonUpgradeRecord) -> Result<()> {
    anyhow::ensure!(
        record.version == SUPERVISOR_RECORD_VERSION,
        "unsupported daemon upgrade record version {}",
        record.version
    );
    anyhow::ensure!(record.incumbent_pid != 0, "invalid incumbent PID");
    anyhow::ensure!(
        record.endpoints.pid_file == record.endpoints.home.join(".finch/daemon.pid"),
        "PID endpoint does not belong to the recorded daemon home"
    );
    anyhow::ensure!(
        record.endpoints.ipc_socket == record.endpoints.home.join(".finch/daemon.sock"),
        "IPC endpoint does not belong to the recorded daemon home"
    );
    anyhow::ensure!(
        hash_file(&record.candidate)? == record.candidate_sha256,
        "candidate digest no longer matches durable intent"
    );
    anyhow::ensure!(
        hash_file(&record.rollback)? == record.rollback_sha256,
        "rollback digest no longer matches durable intent"
    );
    Ok(())
}

fn write_record(path: &Path, record: &DaemonUpgradeRecord) -> Result<()> {
    let parent = path
        .parent()
        .context("daemon upgrade record must have a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upgrade"),
        std::process::id()
    ));
    let bytes = serde_json::to_vec_pretty(record)?;
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn set_phase(record: &mut DaemonUpgradeRecord, path: &Path, phase: SupervisorPhase) -> Result<()> {
    record.phase = phase;
    record.failure = None;
    write_record(path, record)
}

fn spawn_detached_supervisor(record_path: &Path) -> Result<SupervisorLaunch> {
    let executable = std::env::current_exe().context("locate Finch supervisor executable")?;
    let log_path = record_path.with_extension("log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut command = Command::new(executable);
    command
        .arg("__daemon-supervisor")
        .arg("--record")
        .arg(record_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command
        .spawn()
        .context("start detached daemon upgrade supervisor")?;
    Ok(SupervisorLaunch {
        supervisor_pid: child.id(),
        record_path: record_path.to_path_buf(),
    })
}

fn spawn_production(
    binary: &Path,
    endpoints: &ProductionDaemonEndpoints,
    label: &str,
) -> Result<u32> {
    let log_path = endpoints
        .home
        .join(format!(".finch/daemon-upgrade-{label}.log"));
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut command = Command::new(binary);
    command
        .arg("daemon")
        .arg("--bind")
        .arg(endpoints.bind.to_string())
        .env("HOME", &endpoints.home)
        .env("USERPROFILE", &endpoints.home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command.spawn()?.id())
}

async fn wait_for_production_health(
    expected_pid: u32,
    expected_sha256: &str,
    endpoints: &ProductionDaemonEndpoints,
) -> Result<()> {
    let base_url = format!("http://{}", endpoints.bind);
    anyhow::ensure!(
        process_binary_matches(expected_pid, expected_sha256),
        "daemon PID {expected_pid} is not the recorded binary image"
    );
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if !process_exists(expected_pid) {
            anyhow::bail!("daemon PID {expected_pid} exited before becoming healthy");
        }
        let pid_matches = std::fs::read_to_string(&endpoints.pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            == Some(expected_pid);
        if pid_matches
            && super::spawn::health_check_succeeds(&base_url).await
            && ipc_health_succeeds(endpoints.ipc_socket.clone()).await
        {
            return Ok(());
        }
    }
    anyhow::bail!(
        "daemon PID {expected_pid} did not pass PID, HTTP, and Cap'n Proto IPC health within 15 seconds"
    )
}

async fn discover_spawned_pid(
    endpoints: &ProductionDaemonEndpoints,
    expected_sha256: &str,
) -> Option<u32> {
    // Covers a supervisor crash between spawn(2) and the subsequent durable
    // phase/PID write. Give the daemon a brief chance to publish its PID file.
    for _ in 0..8 {
        let pid = std::fs::read_to_string(&endpoints.pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if let Some(pid) = pid {
            if process_exists(pid) && process_binary_matches(pid, expected_sha256) {
                return Some(pid);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

fn process_binary_matches(pid: u32, expected_sha256: &str) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    system
        .process(Pid::from(pid as usize))
        .and_then(|process| process.exe())
        .and_then(|path| hash_file(path).ok())
        .is_some_and(|digest| digest == expected_sha256)
}

async fn ipc_health_succeeds(socket: PathBuf) -> bool {
    let verifier = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async move {
            tokio::time::timeout(Duration::from_secs(2), async move {
                let client = crate::ipc::IpcClient::connect_path(socket).await?;
                drop(client);
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("production IPC protocol handshake timed out")??;
            Ok::<_, anyhow::Error>(())
        })
    });
    matches!(
        tokio::time::timeout(Duration::from_secs(2), verifier).await,
        Ok(Ok(Ok(())))
    )
}

async fn stop_pid(pid: u32) -> Result<()> {
    if !process_exists(pid) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), Signal::SIGTERM)?;
        for _ in 0..50 {
            if !process_exists(pid) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        kill(Pid::from_raw(pid as i32), Signal::SIGKILL)?;
        for _ in 0..20 {
            if !process_exists(pid) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("PID {pid} remained visible after SIGKILL")
    }
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()?;
        anyhow::ensure!(status.success(), "taskkill failed for PID {pid}");
        Ok(())
    }
}

async fn stop_pid_if_binary(pid: u32, expected_sha256: &str) -> Result<()> {
    if !process_exists(pid) {
        return Ok(());
    }
    anyhow::ensure!(
        process_binary_matches(pid, expected_sha256),
        "refusing to signal PID {pid}: process image does not match the recorded hash"
    );
    stop_pid(pid).await
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err() {
        return false;
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    system
        .process(Pid::from(pid as usize))
        .map_or(true, |process| {
            !matches!(
                process.status(),
                ProcessStatus::Dead | ProcessStatus::Zombie
            )
        })
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::nothing());
    system.process(Pid::from(pid as usize)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn hermetic_record(root: &Path, phase: SupervisorPhase) -> DaemonUpgradeRecord {
        use std::os::unix::fs::PermissionsExt;
        let candidate = root.join("candidate");
        let rollback = root.join("rollback");
        std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&rollback, "#!/bin/sh\n# rollback\nexit 0\n").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&rollback, std::fs::Permissions::from_mode(0o700)).unwrap();
        DaemonUpgradeRecord {
            version: SUPERVISOR_RECORD_VERSION,
            phase,
            candidate_sha256: hash_file(&candidate).unwrap(),
            rollback_sha256: hash_file(&rollback).unwrap(),
            candidate,
            rollback,
            schema_impact: "none".into(),
            endpoints: ProductionDaemonEndpoints::for_home(
                &root.join("home"),
                "127.0.0.1:39117".parse().unwrap(),
            ),
            incumbent_pid: 424_242,
            candidate_pid: None,
            rollback_pid: None,
            failure: None,
        }
    }

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

    #[cfg(unix)]
    #[test]
    fn record_is_versioned_atomic_and_preserves_exact_handoff_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state/upgrade.json");
        let mut record = hermetic_record(temp.path(), SupervisorPhase::IntentRecorded);
        write_record(&path, &record).unwrap();
        assert_eq!(read_record(&path).unwrap(), record);

        record.phase = SupervisorPhase::CandidateVerifying;
        record.candidate_pid = Some(12_345);
        write_record(&path, &record).unwrap();
        assert_eq!(read_record(&path).unwrap(), record);
        assert!(std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_recovery_is_hermetic_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("upgrade.json");
        let record = hermetic_record(temp.path(), SupervisorPhase::Promoted);
        write_record(&path, &record).unwrap();
        run_supervisor(&path).await.unwrap();
        run_supervisor(&path).await.unwrap();
        assert_eq!(read_record(&path).unwrap().phase, SupervisorPhase::Promoted);
        assert!(!record.endpoints.pid_file.exists());
        assert!(!record.endpoints.ipc_socket.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_fails_closed_when_an_artifact_hash_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("upgrade.json");
        let record = hermetic_record(temp.path(), SupervisorPhase::Promoted);
        write_record(&path, &record).unwrap();
        std::fs::write(&record.candidate, "tampered").unwrap();
        let error = run_supervisor(&path).await.unwrap_err().to_string();
        assert!(error.contains("candidate digest"), "{error}");
        assert!(!record.endpoints.pid_file.exists());
    }
}
