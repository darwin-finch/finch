// Node module — identity, capabilities, and work statistics.
//
// Every finch instance is a node. Nodes have:
//   - A stable UUID (persisted to ~/.finch/node_id)
//   - Capabilities (what models it can run, what RAM it has)
//   - Work statistics (queries processed, latency, local vs. teacher)
//
// This is the foundation for the distributed worker network where old
// laptops accept delegated work and earn reputation.

pub mod identity;
pub mod stats;
pub mod tls;

pub use identity::NodeIdentity;
pub use stats::{WorkStats, WorkTracker};

use crate::models::model_selector::{ModelSelection, ModelSelector};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Opaque disposable state owned by worker-node integration tests.
///
/// Construction rejects a temporary parent inside the user's Finch state,
/// so test seams cannot silently fall back to `~/.finch`.
#[doc(hidden)]
#[derive(Clone)]
pub struct IsolatedNodeTestState {
    directory: Arc<tempfile::TempDir>,
    descriptor: Arc<std::fs::File>,
    // Every clone shares this process-local creation lock. The integration
    // seam never coordinates through a mutable pathname in the state root.
    identity_lock: Arc<std::sync::Mutex<()>>,
}

impl IsolatedNodeTestState {
    pub fn new() -> anyhow::Result<Self> {
        use anyhow::Context as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        let temporary_parent = std::env::temp_dir()
            .canonicalize()
            .context("could not canonicalize the node-test temporary parent")?;
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let home = home
            .canonicalize()
            .context("Cannot canonicalize home directory")?;
        let user_state = home.join(".finch");
        let user_state = user_state.canonicalize().unwrap_or(user_state);
        anyhow::ensure!(
            !temporary_parent.starts_with(&user_state),
            "node-test temporary parent overlaps the user Finch state"
        );
        let directory = tempfile::Builder::new()
            .prefix("finch-node-test.")
            .tempdir_in(&temporary_parent)
            .context("create disposable node-test state")?;
        let descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.path())
            .context("pin disposable node-test state")?;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let pinned = nix::sys::stat::fstat(descriptor.as_raw_fd())?;
        let named = std::fs::symlink_metadata(directory.path())?;
        anyhow::ensure!(
            named.is_dir()
                && !named.file_type().is_symlink()
                && pinned.st_dev as u64 == named.dev()
                && pinned.st_ino as u64 == named.ino(),
            "disposable node-test state identity changed while pinning"
        );
        Ok(Self {
            directory: Arc::new(directory),
            descriptor: Arc::new(descriptor),
            identity_lock: Arc::new(std::sync::Mutex::new(())),
        })
    }

    pub fn load_node_info(&self, has_teacher_api: bool) -> anyhow::Result<NodeInfo> {
        let _guard = self
            .identity_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("isolated node identity lock poisoned"))?;
        NodeInfo::load_from_state_directory(has_teacher_api, &self.descriptor)
    }

    pub fn node_id_exists(&self) -> anyhow::Result<bool> {
        use nix::fcntl::AtFlags;
        use nix::sys::stat::fstatat;
        use std::os::fd::AsRawFd as _;
        match fstatat(
            Some(self.descriptor.as_raw_fd()),
            "node_id",
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => Ok(true),
            Err(nix::errno::Errno::ENOENT) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn seed_node_id_fixture(&self, contents: &[u8]) -> anyhow::Result<()> {
        use nix::fcntl::{openat, OFlag};
        use nix::sys::stat::Mode;
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let fd = openat(
            Some(self.descriptor.as_raw_fd()),
            "node_id",
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?;
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let stat = nix::sys::stat::fstat(file.as_raw_fd())?;
        anyhow::ensure!(
            nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode)
                .contains(nix::sys::stat::SFlag::S_IFREG)
                && stat.st_nlink == 1,
            "node identity fixture must be one regular file"
        );
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    }

    pub fn node_id_fixture_equals(&self, expected: &[u8]) -> anyhow::Result<bool> {
        use nix::fcntl::{openat, OFlag};
        use nix::sys::stat::Mode;
        use std::io::Read as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let fd = openat(
            Some(self.descriptor.as_raw_fd()),
            "node_id",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let stat = nix::sys::stat::fstat(file.as_raw_fd())?;
        anyhow::ensure!(
            nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode)
                .contains(nix::sys::stat::SFlag::S_IFREG)
                && stat.st_nlink == 1,
            "node identity fixture must be one regular file"
        );
        let mut contents = Vec::new();
        file.take((1 << 20) + 1).read_to_end(&mut contents)?;
        anyhow::ensure!(
            contents.len() <= 1 << 20,
            "node identity fixture is too large"
        );
        Ok(contents == expected)
    }

    pub fn symlink_node_id_fixture(&self, target: &std::path::Path) -> anyhow::Result<()> {
        use nix::unistd::symlinkat;
        use std::os::fd::AsRawFd as _;
        symlinkat(target, Some(self.descriptor.as_raw_fd()), "node_id")?;
        Ok(())
    }

    pub fn hardlink_node_id_fixture(&self, source: &std::path::Path) -> anyhow::Result<()> {
        use nix::fcntl::AtFlags;
        use nix::unistd::linkat;
        use std::os::fd::AsRawFd as _;
        linkat(
            None,
            source,
            Some(self.descriptor.as_raw_fd()),
            std::path::Path::new("node_id"),
            AtFlags::empty(),
        )?;
        Ok(())
    }

    pub fn fifo_node_id_fixture(&self) -> anyhow::Result<()> {
        use std::os::fd::AsRawFd as _;
        let name = std::ffi::CString::new("node_id")?;
        let result = unsafe { libc::mkfifoat(self.descriptor.as_raw_fd(), name.as_ptr(), 0o600) };
        if result == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn swap_root_fixture(&self) -> anyhow::Result<IsolatedNodeRootSwap> {
        let original = self.directory.path().to_path_buf();
        let moved = original.with_extension(format!("moved-{}", uuid::Uuid::new_v4().simple()));
        std::fs::rename(&original, &moved)?;
        std::fs::create_dir(&original)?;
        std::fs::write(original.join("external-sentinel"), b"keep external")?;
        Ok(IsolatedNodeRootSwap { original, moved })
    }

    pub(crate) fn descriptor(&self) -> &std::fs::File {
        &self.descriptor
    }
}

#[doc(hidden)]
pub struct IsolatedNodeRootSwap {
    original: std::path::PathBuf,
    moved: std::path::PathBuf,
}

impl IsolatedNodeRootSwap {
    pub fn pinned_node_id_exists(&self) -> bool {
        self.moved.join("node_id").is_file()
    }

    pub fn replacement_node_id_exists(&self) -> bool {
        self.original.join("node_id").exists()
    }

    pub fn external_sentinel_unchanged(&self) -> bool {
        std::fs::read(self.original.join("external-sentinel")).as_deref() == Ok(b"keep external")
    }
}

impl Drop for IsolatedNodeRootSwap {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.original);
        let _ = std::fs::rename(&self.moved, &self.original);
    }
}

/// Collect basic machine specs for registry metadata.
///
/// Returns `(cpu_cores, ram_mb, bench_ms)`, where `bench_ms` is the time for
/// ten million wrapping integer additions. This belongs to node discovery,
/// not either Finch language runtime.
pub fn collect_machine_specs() -> (u32, u64, u64) {
    use sysinfo::System;

    let mut system = System::new();
    system.refresh_memory();
    system.refresh_cpu_all();
    let cpu_cores = system.cpus().len() as u32;
    let ram_mb = system.total_memory() / (1024 * 1024);
    let start = std::time::Instant::now();
    let mut accumulator = 0_u64;
    for value in 0_u64..10_000_000 {
        accumulator = accumulator.wrapping_add(value);
    }
    let bench_ms = start.elapsed().as_millis() as u64;
    std::hint::black_box(accumulator);
    (cpu_cores, ram_mb, bench_ms)
}

/// Full description of this node's capabilities. Dynamic capability data is
/// not advertised over mDNS; remote collaborators retrieve it through a
/// Brain-scoped authenticated endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub identity: NodeIdentity,
    pub capabilities: NodeCapabilities,
}

/// What this node can do
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// RAM in GB
    pub ram_gb: usize,
    /// Local model available (None = cloud-only mode)
    pub local_model: Option<String>,
    /// Whether a teacher API is configured
    pub has_teacher_api: bool,
    /// Finch version
    pub version: String,
    /// Operating system
    pub os: String,
}

impl NodeCapabilities {
    pub fn detect(has_teacher_api: bool) -> Self {
        let ram_gb = ModelSelector::get_total_ram_gb();
        let local_model = match ModelSelector::select_for_system() {
            Ok(ModelSelection::Local(size)) => Some(size.description().to_string()),
            _ => None,
        };

        Self {
            ram_gb,
            local_model,
            has_teacher_api,
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
        }
    }

    pub fn is_cloud_only(&self) -> bool {
        self.local_model.is_none()
    }
}

impl NodeInfo {
    pub fn load(has_teacher_api: bool) -> anyhow::Result<Self> {
        Ok(Self {
            identity: NodeIdentity::load_or_create()?,
            capabilities: NodeCapabilities::detect(has_teacher_api),
        })
    }

    /// Load node information using an explicit Finch state directory.
    pub(crate) fn load_from_state_directory(
        has_teacher_api: bool,
        directory: &std::fs::File,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            identity: NodeIdentity::load_or_create_in(directory)?,
            capabilities: NodeCapabilities::detect(has_teacher_api),
        })
    }

    /// One-line summary for status display
    pub fn summary(&self) -> String {
        let model = self
            .capabilities
            .local_model
            .as_deref()
            .unwrap_or("cloud-only");
        format!(
            "node:{} | {} | {}GB RAM | {}",
            self.identity.short_id(),
            model,
            self.capabilities.ram_gb,
            self.capabilities.os,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_capabilities_detect() {
        let caps = NodeCapabilities::detect(false);
        assert!(caps.ram_gb >= 1);
        assert!(!caps.version.is_empty());
        assert!(!caps.os.is_empty());
    }

    #[test]
    fn test_node_capabilities_cloud_only_when_no_model() {
        let caps = NodeCapabilities {
            ram_gb: 1,
            local_model: None,
            has_teacher_api: true,
            version: "test".to_string(),
            os: "test".to_string(),
        };
        assert!(caps.is_cloud_only());
    }

    #[test]
    fn machine_specs_are_available_without_a_language_runtime() {
        let (cpu_cores, ram_mb, _bench_ms) = collect_machine_specs();
        assert!(cpu_cores > 0);
        assert!(ram_mb > 0);
    }
}
