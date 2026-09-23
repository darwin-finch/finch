use super::identity::{ResolvedSource, SourceReadOutcome, SourceSkipReason, MAX_SOURCE_BYTES};
use super::outline::{outline_source, span_line_coordinates, MAX_LABEL_BYTES, MAX_OUTLINE_RECORDS};
use super::{
    OutlineRecord, OutlineResult, RetrievalProvenance, SourceIdentity, SourceResolver, SourceSpan,
};
use anyhow::{bail, Context, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
#[cfg(unix)]
use cap_std::fs::DirBuilderExt;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use fs2::FileExt;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

const STORAGE_SCHEMA_VERSION: u32 = 1;
const OUTLINE_SERIALIZATION_VERSION: u32 = 1;
const OUTLINE_ALGORITHM_VERSION: &str = "source-outline-v1";
const MAX_INDEX_FILES: usize = 20_000;
const MAX_INDEX_DIRECTORIES: usize = 10_000;
const MAX_INDEX_RECORDS: usize = 200_000;
const MAX_DIRECTORY_CHILDREN: usize = 2_000;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_AGENT_LEAD_BYTES: usize = 512;
const MAX_IGNORE_CONTROL_BYTES: u64 = 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_WORKSPACE_CACHES: usize = 8;
const STATE_LOCK_LEAF: &str = "source-index-state.lock";

/// A body-free outline stored in a repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedOutline {
    pub outline: OutlineResult,
}

/// The one intentional source-body routing hint: the first prose paragraph of AGENTS.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLead {
    pub source: SourceIdentity,
    pub span: SourceSpan,
    pub text: String,
}

/// Kind of a direct child in one directory menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryChildKind {
    Directory,
    File,
}

/// One direct child available to a later sibling-hop router.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryChild {
    pub name: String,
    pub kind: DirectoryChildKind,
}

/// Stable direct-child menu and optional capsule lead for one directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryRecord {
    pub path: String,
    pub children: Vec<DirectoryChild>,
    pub agent_lead: Option<AgentLead>,
}

/// One complete, bounded repository routing snapshot.
///
/// The snapshot is an optimistic view, not a filesystem transaction. A caller
/// must rebuild when `RepositoryIndexer::is_snapshot_current` returns false and
/// must still consume every selected span through `SourceResolver::read_span`.
/// A mutation after freshness validation may add an omitted candidate, but it
/// cannot make a generation-bound selected leaf return new bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub workspace_namespace_sha256: String,
    pub repository_revision: String,
    pub manifest_sha256: String,
    pub directories: Vec<DirectoryRecord>,
    pub files: Vec<IndexedOutline>,
}

impl RepositorySnapshot {
    /// Return the direct children at the repository root.
    pub fn root_children(&self) -> &[DirectoryChild] {
        self.directory("")
            .map(|directory| directory.children.as_slice())
            .unwrap_or(&[])
    }

    /// Find one normalized workspace-relative directory menu.
    pub fn directory(&self, path: &str) -> Option<&DirectoryRecord> {
        self.directories
            .binary_search_by(|record| record.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.directories[index])
    }

    /// Find one normalized workspace-relative file outline.
    pub fn file(&self, path: &str) -> Option<&IndexedOutline> {
        self.files
            .binary_search_by(|record| record.outline.source.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.files[index])
    }
}

/// Observable work avoided or performed by one index build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepositoryBuildStats {
    pub indexed_files: usize,
    pub parsed_files: usize,
    pub reused_files: usize,
    pub skipped_non_text_files: usize,
}

/// Result of atomically publishing one repository generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryBuild {
    pub snapshot: RepositorySnapshot,
    pub stats: RepositoryBuildStats,
}

/// Typed cache incompatibility plus bounded indexing failures.
#[derive(Debug, Error)]
pub enum RepositoryIndexError {
    #[error(
        "source-index cache is incompatible (storage {found_schema}, outline serialization {found_serialization}, algorithm {found_outline}); expected storage {expected_schema}, outline serialization {expected_serialization}, algorithm {expected_outline}; rebuild it explicitly"
    )]
    IncompatibleCache {
        found_schema: u32,
        found_serialization: u32,
        found_outline: String,
        expected_schema: u32,
        expected_serialization: u32,
        expected_outline: &'static str,
    },
    #[error(
        "source-index published a complete cache image, but directory durability could not be confirmed: {source}"
    )]
    PublishedButNotDurable {
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type IndexResult<T> = std::result::Result<T, RepositoryIndexError>;

/// Capability-rooted, versioned on-disk cache for one workspace namespace.
pub struct RepositoryCache {
    directory: Dir,
    workspace_namespace_sha256: String,
    cache_leaf: String,
    temporary_leaf: String,
    lock_leaf: String,
    #[cfg(test)]
    fail_directory_sync: bool,
}

impl RepositoryCache {
    /// Create a missing private state leaf beneath an existing parent capability,
    /// then open it and enforce the shared derived-cache quota.
    pub fn prepare(
        state_directory: impl AsRef<Path>,
        resolver: &SourceResolver,
    ) -> IndexResult<Self> {
        Self::prepare_inner(state_directory.as_ref(), resolver, None)
    }

    pub(crate) fn prepare_bounded(
        state_directory: impl AsRef<Path>,
        resolver: &SourceResolver,
        deadline: Instant,
    ) -> IndexResult<Self> {
        Self::prepare_inner(state_directory.as_ref(), resolver, Some(deadline))
    }

    fn prepare_inner(
        state_directory: &Path,
        resolver: &SourceResolver,
        deadline: Option<Instant>,
    ) -> IndexResult<Self> {
        let leaf = state_directory.file_name().context(
            "source-index state path must name a directory rather than a filesystem root",
        )?;
        let parent_path = state_directory
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .with_context(|| {
                format!(
                    "source-index state parent must already exist: {}",
                    state_directory.display()
                )
            })?;
        let parent =
            Dir::open_ambient_dir(&parent_path, ambient_authority()).with_context(|| {
                format!(
                    "failed to capability-open source-index state parent {}",
                    parent_path.display()
                )
            })?;
        match parent.open_dir_nofollow(leaf) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                match parent.create_dir_with(leaf, &builder) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        parent.open_dir_nofollow(leaf).with_context(|| {
                            format!(
                                "concurrently created source-index state path is not a real directory: {}",
                                state_directory.display()
                            )
                        })?;
                    }
                    Err(error) => {
                        return Err(error)
                            .with_context(|| {
                                format!(
                                    "failed to create private source-index state directory {}",
                                    state_directory.display()
                                )
                            })
                            .map_err(Into::into)
                    }
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| {
                        format!(
                            "source-index state path must be a real non-symlink directory: {}",
                            state_directory.display()
                        )
                    })
                    .map_err(Into::into)
            }
        }
        let cache = Self::open(state_directory, resolver)?;
        let state_lock = cache.open_named_lock(STATE_LOCK_LEAF, "source-index state lock")?;
        lock_exclusive_bounded(&state_lock, deadline, "source-index state quota lock")?;
        cache.ensure_state_quota()?;
        FileExt::unlock(&state_lock).context("failed to release source-index state quota lock")?;
        Ok(cache)
    }

    /// Open an existing canonical, owner-only state directory.
    pub fn open(state_directory: impl AsRef<Path>, resolver: &SourceResolver) -> IndexResult<Self> {
        let workspace_namespace_sha256 = resolver.workspace_namespace_sha256();
        ensure_namespace(workspace_namespace_sha256)?;
        let state_directory = state_directory.as_ref();
        let leaf = state_directory.file_name().context(
            "source-index state path must name an existing directory rather than a filesystem root",
        )?;
        let parent = state_directory
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to resolve source-index state parent for {}",
                    state_directory.display()
                )
            })?;
        let parent = Dir::open_ambient_dir(&parent, ambient_authority()).with_context(|| {
            format!(
                "failed to capability-open source-index state parent for {}",
                state_directory.display()
            )
        })?;
        let directory = parent.open_dir_nofollow(leaf).with_context(|| {
            format!(
                "source-index state path must be a real non-symlink directory: {}",
                state_directory.display()
            )
        })?;
        let metadata = directory
            .try_clone()
            .context("failed to clone source-index state directory")?
            .into_std_file()
            .metadata()
            .context("failed to inspect opened source-index state directory")?;
        ensure_owner_only_metadata(&metadata, "source-index state directory")?;
        Ok(Self {
            directory,
            workspace_namespace_sha256: workspace_namespace_sha256.to_string(),
            cache_leaf: format!("source-index-{workspace_namespace_sha256}.json"),
            temporary_leaf: format!("source-index-{workspace_namespace_sha256}.tmp"),
            lock_leaf: format!("source-index-{workspace_namespace_sha256}.lock"),
            #[cfg(test)]
            fail_directory_sync: false,
        })
    }

    /// Read the last complete compatible snapshot, if one exists.
    pub fn load(&self) -> IndexResult<Option<RepositorySnapshot>> {
        self.load_bounded(None)
    }

    pub(crate) fn load_with_deadline(
        &self,
        deadline: Instant,
    ) -> IndexResult<Option<RepositorySnapshot>> {
        self.load_bounded(Some(deadline))
    }

    fn load_bounded(&self, deadline: Option<Instant>) -> IndexResult<Option<RepositorySnapshot>> {
        let lock = self.open_lock()?;
        lock_shared_bounded(&lock, deadline, "source-index read lock")?;
        let image = self.read_image()?;
        FileExt::unlock(&lock).context("failed to release source-index read lock")?;
        Ok(image.map(|image| image.snapshot))
    }

    fn open_lock(&self) -> Result<std::fs::File> {
        self.open_named_lock(&self.lock_leaf, "source-index lock")
    }

    fn open_named_lock(&self, leaf: &str, label: &str) -> Result<std::fs::File> {
        // `cap-fs-ext` implements create-with-no-follow with a checked open;
        // two first creators can make one checked attempt observe ENOENT.
        // Retrying the same bounded leaf closes that harmless creation race.
        let mut last_error = None;
        for _ in 0..3 {
            match self.open_private_leaf(leaf, true, false) {
                Ok(file) => {
                    ensure_private_file(&file, label)?;
                    return Ok(file);
                }
                Err(error)
                    if error
                        .root_cause()
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
                {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("lock retry records an error"))
    }

    fn ensure_state_quota(&self) -> IndexResult<()> {
        let mut cache_images = 0usize;
        let mut total_bytes = 0u64;
        let mut current_exists = false;
        for entry in self
            .directory
            .entries()
            .context("failed to enumerate source-index state directory")?
        {
            let entry = entry.context("failed to inspect source-index state entry")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("source-index-")
                || !(name.ends_with(".json") || name.ends_with(".tmp") || name.ends_with(".lock"))
            {
                continue;
            }
            let metadata = self
                .directory
                .symlink_metadata(&name)
                .with_context(|| format!("failed to inspect source-index state leaf {name}"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(anyhow::anyhow!(
                    "source-index state leaf {name} must be a regular non-symlink file"
                )
                .into());
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .context("source-index state byte count overflowed")?;
            if name.ends_with(".json") {
                cache_images += 1;
                current_exists |= name == self.cache_leaf;
            }
        }
        if total_bytes > MAX_STATE_BYTES {
            return Err(anyhow::anyhow!(
                "source-index state exceeds {} bytes; remove unused derived caches from ~/.finch/source-index",
                MAX_STATE_BYTES
            )
            .into());
        }
        if cache_images > MAX_WORKSPACE_CACHES
            || (!current_exists && cache_images == MAX_WORKSPACE_CACHES)
        {
            return Err(anyhow::anyhow!(
                "source-index state already contains {MAX_WORKSPACE_CACHES} workspace caches; remove unused derived caches from ~/.finch/source-index"
            )
            .into());
        }
        Ok(())
    }

    fn open_private_leaf(&self, leaf: &str, create: bool, truncate: bool) -> Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(create)
            .truncate(truncate);
        options.follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.mode(0o600);
        self.directory
            .open_with(leaf, &options)
            .with_context(|| format!("failed to open private source-index leaf {leaf}"))
            .map(cap_std::fs::File::into_std)
    }

    fn sync_directory(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            // `open_dir_nofollow` may use Linux O_PATH. That descriptor is
            // sufficient for capability-relative traversal and fstat (which is
            // why open-time metadata checks succeed), but fsync returns EBADF.
            // Reopen only `.` beneath the held capability with read authority
            // so the published rename can be durably synced.
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            return self
                .directory
                .open_with(".", &options)
                .map(cap_std::fs::File::into_std)?
                .sync_all();
        }
        #[cfg(not(unix))]
        self.directory.try_clone()?.into_std_file().sync_all()
    }

    fn read_image(&self) -> IndexResult<Option<CacheImage>> {
        let file = match self.open_private_leaf(&self.cache_leaf, false, false) {
            Ok(file) => file,
            Err(error)
                if error
                    .root_cause()
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error.into()),
        };
        ensure_private_file(&file, "source-index cache")?;
        let length = file
            .metadata()
            .context("failed to inspect source-index cache")?
            .len();
        if length > MAX_CACHE_BYTES {
            return Err(
                anyhow::anyhow!("source-index cache exceeds {} bytes", MAX_CACHE_BYTES).into(),
            );
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take(MAX_CACHE_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read source-index cache")?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return Err(
                anyhow::anyhow!("source-index cache exceeds {} bytes", MAX_CACHE_BYTES).into(),
            );
        }
        let version: CacheVersionEnvelope = serde_json::from_slice(&bytes)
            .context("failed to decode source-index cache version envelope")?;
        if version.storage_schema_version != Some(STORAGE_SCHEMA_VERSION)
            || version.outline_serialization_version != Some(OUTLINE_SERIALIZATION_VERSION)
            || version.outline_algorithm_version.as_deref() != Some(OUTLINE_ALGORITHM_VERSION)
        {
            return Err(RepositoryIndexError::IncompatibleCache {
                found_schema: version.storage_schema_version.unwrap_or(0),
                found_serialization: version.outline_serialization_version.unwrap_or(0),
                found_outline: version
                    .outline_algorithm_version
                    .unwrap_or_else(|| "<missing>".to_string()),
                expected_schema: STORAGE_SCHEMA_VERSION,
                expected_serialization: OUTLINE_SERIALIZATION_VERSION,
                expected_outline: OUTLINE_ALGORITHM_VERSION,
            });
        }
        let image: CacheImage =
            serde_json::from_slice(&bytes).context("failed to decode source-index cache image")?;
        validate_cache_image(&image, &self.workspace_namespace_sha256)?;
        Ok(Some(image))
    }

    fn publish_image(&self, image: &CacheImage, deadline: Option<Instant>) -> IndexResult<()> {
        let bytes = serde_json::to_vec(image).context("failed to encode source-index cache")?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return Err(anyhow::anyhow!(
                "source-index cache would exceed {} bytes",
                MAX_CACHE_BYTES
            )
            .into());
        }
        let state_lock = self.open_named_lock(STATE_LOCK_LEAF, "source-index state lock")?;
        lock_exclusive_bounded(&state_lock, deadline, "source-index state publication lock")?;
        let result = self.publish_image_under_state_lock(&bytes);
        FileExt::unlock(&state_lock)
            .context("failed to release source-index state publication lock")?;
        result
    }

    fn publish_image_under_state_lock(&self, bytes: &[u8]) -> IndexResult<()> {
        self.ensure_state_quota()?;
        let mut projected_state_bytes = bytes.len() as u64;
        for entry in self
            .directory
            .entries()
            .context("failed to enumerate source-index state before publication")?
        {
            let entry = entry.context("failed to inspect source-index state before publication")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // The old image and the complete temporary replacement coexist
            // until rename, so peak quota accounting must retain the old image.
            if name == self.temporary_leaf {
                continue;
            }
            if !name.starts_with("source-index-")
                || !(name.ends_with(".json") || name.ends_with(".tmp") || name.ends_with(".lock"))
            {
                continue;
            }
            let metadata = self
                .directory
                .symlink_metadata(name)
                .with_context(|| format!("failed to inspect source-index state leaf {name}"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(anyhow::anyhow!(
                    "source-index state leaf {name} must be a regular non-symlink file"
                )
                .into());
            }
            projected_state_bytes = projected_state_bytes
                .checked_add(metadata.len())
                .context("source-index projected state byte count overflowed")?;
        }
        if projected_state_bytes > MAX_STATE_BYTES {
            return Err(anyhow::anyhow!(
                "source-index publication would exceed {} shared state bytes; remove unused derived caches from ~/.finch/source-index",
                MAX_STATE_BYTES
            )
            .into());
        }
        let mut temporary = self.open_private_leaf(&self.temporary_leaf, true, true)?;
        ensure_private_file(&temporary, "source-index temporary cache")?;
        temporary
            .write_all(&bytes)
            .context("failed to write source-index temporary cache")?;
        temporary
            .sync_all()
            .context("failed to sync source-index temporary cache")?;
        drop(temporary);
        self.directory
            .rename(&self.temporary_leaf, &self.directory, &self.cache_leaf)
            .context("failed to atomically publish source-index cache")?;
        // Rename is the publication commit point. Report a distinct error after
        // it: readers already see a complete new image, but crash durability was
        // not confirmed and callers must not retry as though publication failed.
        #[cfg(test)]
        if self.fail_directory_sync {
            return Err(RepositoryIndexError::PublishedButNotDurable {
                source: std::io::Error::other("injected directory sync failure"),
            });
        }
        self.sync_directory()
            .map_err(|source| RepositoryIndexError::PublishedButNotDurable { source })?;
        Ok(())
    }
}

fn lock_exclusive_bounded(
    file: &std::fs::File,
    deadline: Option<Instant>,
    label: &str,
) -> Result<()> {
    let Some(deadline) = deadline else {
        FileExt::lock_exclusive(file).with_context(|| format!("failed to take {label}"))?;
        return Ok(());
    };
    lock_bounded(file, deadline, label, |file| {
        FileExt::try_lock_exclusive(file)
    })
}

fn lock_shared_bounded(file: &std::fs::File, deadline: Option<Instant>, label: &str) -> Result<()> {
    let Some(deadline) = deadline else {
        FileExt::lock_shared(file).with_context(|| format!("failed to take {label}"))?;
        return Ok(());
    };
    lock_bounded(file, deadline, label, |file| FileExt::try_lock_shared(file))
}

fn lock_bounded(
    file: &std::fs::File,
    deadline: Instant,
    label: &str,
    try_lock: impl Fn(&std::fs::File) -> std::io::Result<()>,
) -> Result<()> {
    loop {
        match try_lock(file) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    bail!("deadline expired while waiting for {label}");
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(std::time::Duration::from_millis(10)),
                );
            }
            Err(error) => return Err(error).with_context(|| format!("failed to take {label}")),
        }
    }
}

/// Deterministic repository snapshot builder over a source resolver and cache.
pub struct RepositoryIndexer {
    resolver: SourceResolver,
    cache: RepositoryCache,
    #[cfg(test)]
    before_validation: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl RepositoryIndexer {
    pub fn new(resolver: SourceResolver, cache: RepositoryCache) -> IndexResult<Self> {
        if cache.workspace_namespace_sha256 != resolver.workspace_namespace_sha256() {
            return Err(anyhow::anyhow!(
                "source-index resolver and cache belong to different workspace namespaces"
            )
            .into());
        }
        Ok(Self {
            resolver,
            cache,
            #[cfg(test)]
            before_validation: None,
        })
    }

    /// Reuse a compatible cache and atomically publish the current generation.
    pub fn build(&self) -> IndexResult<RepositoryBuild> {
        self.build_inner(false)
    }

    /// Build a fresh cache image while preserving incompatible old bytes until publication.
    pub fn rebuild(&self) -> IndexResult<RepositoryBuild> {
        self.build_inner(true)
    }

    pub(crate) fn build_bounded(
        &self,
        deadline: Instant,
        max_source_bytes: u64,
    ) -> IndexResult<RepositoryBuild> {
        self.build_inner_bounded(false, Some((deadline, max_source_bytes)))
    }

    pub(crate) fn rebuild_bounded(
        &self,
        deadline: Instant,
        max_source_bytes: u64,
    ) -> IndexResult<RepositoryBuild> {
        self.build_inner_bounded(true, Some((deadline, max_source_bytes)))
    }

    /// Check whether paths, ignore controls, dispositions, and bytes still match.
    ///
    /// A false result requires rebuilding before routing. A true result is only
    /// an optimistic observation; selected leaves still require a
    /// generation-bound `SourceResolver::read_span` call.
    pub fn is_snapshot_current(&self, snapshot: &RepositorySnapshot) -> IndexResult<bool> {
        self.is_snapshot_current_bounded(snapshot, None)
    }

    pub(crate) fn is_snapshot_current_with_budget(
        &self,
        snapshot: &RepositorySnapshot,
        deadline: Instant,
        max_source_bytes: u64,
    ) -> IndexResult<bool> {
        self.is_snapshot_current_bounded(snapshot, Some((deadline, max_source_bytes)))
    }

    fn is_snapshot_current_bounded(
        &self,
        snapshot: &RepositorySnapshot,
        budget: Option<(Instant, u64)>,
    ) -> IndexResult<bool> {
        if snapshot.workspace_namespace_sha256 != self.resolver.workspace_namespace_sha256() {
            return Ok(false);
        }
        let manifest = RepositoryManifest::capture(&self.resolver)?;
        if manifest.revision != snapshot.repository_revision {
            return Ok(false);
        }
        let mut consumed = 0u64;
        Ok(
            capture_generation_sha256_bounded(&self.resolver, &manifest, budget, &mut consumed)?
                == snapshot.manifest_sha256,
        )
    }

    #[cfg(test)]
    fn with_before_validation_hook(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.before_validation = Some(Arc::new(hook));
        self
    }

    fn build_inner(&self, ignore_incompatible: bool) -> IndexResult<RepositoryBuild> {
        self.build_inner_bounded(ignore_incompatible, None)
    }

    fn build_inner_bounded(
        &self,
        ignore_incompatible: bool,
        budget: Option<(Instant, u64)>,
    ) -> IndexResult<RepositoryBuild> {
        let lock = self.cache.open_lock()?;
        let deadline = budget.map(|(deadline, _)| deadline);
        lock_exclusive_bounded(&lock, deadline, "source-index build lock")?;
        let previous = match self.cache.read_image() {
            Ok(image) => image,
            Err(RepositoryIndexError::IncompatibleCache { .. }) if ignore_incompatible => None,
            Err(error) => return Err(error),
        };
        let result = self.build_locked(previous.as_ref(), budget)?;
        let image = CacheImage {
            storage_schema_version: STORAGE_SCHEMA_VERSION,
            outline_serialization_version: OUTLINE_SERIALIZATION_VERSION,
            outline_algorithm_version: OUTLINE_ALGORITHM_VERSION.to_string(),
            snapshot: result.snapshot.clone(),
            parse_entries: result.parse_entries,
        };
        self.cache.publish_image(&image, deadline)?;
        FileExt::unlock(&lock).context("failed to release source-index build lock")?;
        Ok(RepositoryBuild {
            snapshot: result.snapshot,
            stats: result.stats,
        })
    }

    fn build_locked(
        &self,
        previous: Option<&CacheImage>,
        budget: Option<(Instant, u64)>,
    ) -> IndexResult<PendingBuild> {
        let manifest = RepositoryManifest::capture(&self.resolver)?;
        ensure_file_count(manifest.paths.len())?;
        let mut files = Vec::new();
        let mut leads = BTreeMap::new();
        let mut parse_entries = BTreeMap::new();
        let mut parsed_files = 0;
        let mut reused_files = 0;
        let mut skipped_non_text_files = 0;
        let mut total_records = 0;
        let mut entry_generations = Vec::with_capacity(manifest.paths.len());
        let mut consumed_source_bytes = 0u64;

        for path in &manifest.paths {
            check_repository_work_budget(budget, consumed_source_bytes)?;
            let source = match self
                .resolver
                .read_indexable(Path::new(path), &manifest.revision)?
            {
                SourceReadOutcome::Text(source) => source,
                SourceReadOutcome::Skipped { reason, bytes_read } => {
                    consumed_source_bytes = consumed_source_bytes
                        .checked_add(bytes_read)
                        .context("source-index work byte count overflowed")?;
                    check_repository_work_budget(budget, consumed_source_bytes)?;
                    entry_generations.push(RepositoryEntryGeneration::skipped(path, &reason));
                    skipped_non_text_files += 1;
                    continue;
                }
            };
            consumed_source_bytes = consumed_source_bytes
                .checked_add(source.identity.byte_len as u64)
                .context("source-index work byte count overflowed")?;
            check_repository_work_budget(budget, consumed_source_bytes)?;
            entry_generations.push(RepositoryEntryGeneration::text(
                path,
                &source.identity.content_sha256,
            ));
            if path.ends_with("/AGENTS.md") || path == "AGENTS.md" {
                if let Some(lead) = extract_agent_lead(&source) {
                    let directory = Path::new(path)
                        .parent()
                        .and_then(Path::to_str)
                        .unwrap_or_default()
                        .replace('\\', "/");
                    leads.insert(directory, lead);
                }
            }
            let key = ParseCacheKey::for_source(&source).encoded();
            let cached = previous.and_then(|image| image.parse_entries.get(&key));
            let outline = if let Some(cached) = cached {
                reused_files += 1;
                cached.with_identity(source.identity.clone())
            } else {
                parsed_files += 1;
                outline_source(source)?
            };
            total_records = add_record_count(total_records, outline.records.len())?;
            parse_entries.insert(key, CachedOutline::from_outline(&outline));
            files.push(IndexedOutline { outline });
        }

        files.sort_by(|left, right| left.outline.source.path.cmp(&right.outline.source.path));
        let directories = build_directories(&files, leads)?;
        let snapshot = RepositorySnapshot {
            workspace_namespace_sha256: self.resolver.workspace_namespace_sha256().to_string(),
            repository_revision: manifest.revision.clone(),
            manifest_sha256: generation_sha256(&manifest, &entry_generations),
            directories,
            files,
        };

        #[cfg(test)]
        if let Some(hook) = &self.before_validation {
            hook();
        }
        let validation_manifest = RepositoryManifest::capture(&self.resolver)?;
        if validation_manifest != manifest {
            return Err(
                anyhow::anyhow!("repository manifest changed during source-index build").into(),
            );
        }
        if capture_generation_sha256_bounded(
            &self.resolver,
            &validation_manifest,
            budget,
            &mut consumed_source_bytes,
        )? != snapshot.manifest_sha256
        {
            return Err(anyhow::anyhow!(
                "repository source generation changed during source-index build"
            )
            .into());
        }

        Ok(PendingBuild {
            stats: RepositoryBuildStats {
                indexed_files: snapshot.files.len(),
                parsed_files,
                reused_files,
                skipped_non_text_files,
            },
            snapshot,
            parse_entries,
        })
    }
}

fn ensure_file_count(count: usize) -> IndexResult<()> {
    if count > MAX_INDEX_FILES {
        return Err(anyhow::anyhow!(
            "repository has more than {MAX_INDEX_FILES} indexable path entries"
        )
        .into());
    }
    Ok(())
}

fn add_record_count(current: usize, additional: usize) -> IndexResult<usize> {
    let total = current
        .checked_add(additional)
        .context("repository outline record count overflowed")?;
    if total > MAX_INDEX_RECORDS {
        return Err(
            anyhow::anyhow!("repository outlines exceed {MAX_INDEX_RECORDS} records").into(),
        );
    }
    Ok(total)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryManifest {
    revision: String,
    paths: Vec<String>,
    sha256: String,
}

impl RepositoryManifest {
    fn capture(resolver: &SourceResolver) -> Result<Self> {
        let revision = resolver
            .repository_revision()
            .context("source-index repository has no readable Git HEAD")?;
        let tracked = git_output(
            resolver.workspace_root(),
            &["ls-files", "--cached", "--stage", "-z"],
            false,
        )?;
        let untracked = git_output(
            resolver.workspace_root(),
            &[
                "-c",
                "core.excludesFile=/dev/null",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            true,
        )?;
        let mut paths = BTreeSet::new();
        for entry in tracked
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let tab = entry
                .iter()
                .position(|byte| *byte == b'\t')
                .context("malformed git ls-files --stage output")?;
            let header =
                std::str::from_utf8(&entry[..tab]).context("non-UTF-8 git stage metadata")?;
            let mut fields = header.split_ascii_whitespace();
            let mode = fields.next().context("missing git file mode")?;
            let _object = fields.next().context("missing git object id")?;
            let stage = fields.next().context("missing git index stage")?;
            if stage != "0" {
                bail!("source-index refuses an unmerged Git index");
            }
            if matches!(mode, "120000" | "160000") {
                continue;
            }
            paths.insert(normalize_manifest_path(&entry[tab + 1..])?);
        }
        for entry in untracked
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let path = normalize_manifest_path(entry)?;
            if resolver.manifest_entry_is_regular(Path::new(&path))? {
                paths.insert(path);
            }
        }
        let paths = paths.into_iter().collect::<Vec<_>>();
        if paths.len() > MAX_INDEX_FILES {
            bail!("repository has more than {MAX_INDEX_FILES} indexable path entries");
        }
        let ignore_controls = ignore_control_hashes(resolver, &paths)?;
        let mut hasher = Sha256::new();
        hasher.update(b"finch-source-manifest-v1\0");
        hasher.update(revision.as_bytes());
        hasher.update([0]);
        for path in &paths {
            hasher.update(path.as_bytes());
            hasher.update([0]);
        }
        for (path, digest) in ignore_controls {
            hasher.update([0]);
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(digest.as_bytes());
        }
        Ok(Self {
            revision,
            paths,
            sha256: format!("{:x}", hasher.finalize()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryEntryGeneration {
    path: String,
    disposition: String,
}

impl RepositoryEntryGeneration {
    fn text(path: &str, content_sha256: &str) -> Self {
        Self {
            path: path.to_string(),
            disposition: format!("text:{content_sha256}"),
        }
    }

    fn skipped(path: &str, reason: &SourceSkipReason) -> Self {
        let disposition = match reason {
            SourceSkipReason::Oversized { bytes } => format!("oversized:{bytes}"),
            SourceSkipReason::NonUtf8 => "non_utf8".to_string(),
            SourceSkipReason::Symlink => "symlink".to_string(),
        };
        Self {
            path: path.to_string(),
            disposition,
        }
    }
}

fn capture_generation_sha256_bounded(
    resolver: &SourceResolver,
    manifest: &RepositoryManifest,
    budget: Option<(Instant, u64)>,
    consumed_source_bytes: &mut u64,
) -> Result<String> {
    let mut entries = Vec::with_capacity(manifest.paths.len());
    for path in &manifest.paths {
        check_repository_work_budget(budget, *consumed_source_bytes)?;
        match resolver.read_indexable(Path::new(path), &manifest.revision)? {
            SourceReadOutcome::Text(source) => {
                *consumed_source_bytes = consumed_source_bytes
                    .checked_add(source.identity.byte_len as u64)
                    .context("source-index work byte count overflowed")?;
                check_repository_work_budget(budget, *consumed_source_bytes)?;
                entries.push(RepositoryEntryGeneration::text(
                    path,
                    &source.identity.content_sha256,
                ));
            }
            SourceReadOutcome::Skipped { reason, bytes_read } => {
                *consumed_source_bytes = consumed_source_bytes
                    .checked_add(bytes_read)
                    .context("source-index work byte count overflowed")?;
                check_repository_work_budget(budget, *consumed_source_bytes)?;
                entries.push(RepositoryEntryGeneration::skipped(path, &reason))
            }
        }
    }
    Ok(generation_sha256(manifest, &entries))
}

fn check_repository_work_budget(budget: Option<(Instant, u64)>, consumed: u64) -> Result<()> {
    let Some((deadline, max_source_bytes)) = budget else {
        return Ok(());
    };
    if Instant::now() >= deadline {
        bail!("source-index work deadline expired");
    }
    if consumed > max_source_bytes {
        bail!("source-index work exceeds {max_source_bytes} aggregate source bytes");
    }
    Ok(())
}

fn generation_sha256(
    manifest: &RepositoryManifest,
    entries: &[RepositoryEntryGeneration],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"finch-source-generation-v1\0");
    hasher.update(manifest.sha256.as_bytes());
    for entry in entries {
        hasher.update([0]);
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.disposition.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn ignore_control_hashes(
    resolver: &SourceResolver,
    manifest_paths: &[String],
) -> Result<Vec<(String, String)>> {
    let mut controls = Vec::new();
    let mut directories = BTreeSet::from([String::new()]);
    for manifest_path in manifest_paths {
        let mut parent = Path::new(manifest_path).parent().map(Path::to_path_buf);
        while let Some(directory_path) = parent {
            let directory = directory_path
                .to_str()
                .context("repository directory is not UTF-8")?
                .replace('\\', "/");
            directories.insert(directory);
            parent = directory_path.parent().map(Path::to_path_buf);
        }
        if directories.len() > MAX_INDEX_DIRECTORIES {
            bail!("repository has more than {MAX_INDEX_DIRECTORIES} participating directories");
        }
    }
    let mut control_paths = directories
        .into_iter()
        .map(|directory| {
            if directory.is_empty() {
                ".gitignore".to_string()
            } else {
                format!("{directory}/.gitignore")
            }
        })
        .collect::<BTreeSet<_>>();

    // A .gitignore may ignore its own path while still governing siblings.
    // Ask Git for ignored controls, then retain only those whose parent
    // directory is reachable; Git does not read controls below an excluded
    // directory.
    let ignored_controls = git_output(
        resolver.workspace_root(),
        &[
            "-c",
            "core.excludesFile=/dev/null",
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
            ".gitignore",
            "**/.gitignore",
        ],
        true,
    )?;
    for raw in ignored_controls
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let path = normalize_manifest_path(raw)?;
        let parent = Path::new(&path)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or_default();
        if parent.is_empty() || !git_path_is_ignored(resolver.workspace_root(), parent)? {
            control_paths.insert(path);
        }
        if control_paths.len() > MAX_INDEX_DIRECTORIES {
            bail!("repository has more than {MAX_INDEX_DIRECTORIES} participating ignore controls");
        }
    }

    for path in control_paths {
        if let Some(digest) =
            resolver.control_file_sha256(Path::new(&path), MAX_IGNORE_CONTROL_BYTES)?
        {
            controls.push((path, digest));
        }
    }

    let info_exclude = git_output(
        resolver.workspace_root(),
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
        false,
    )?;
    let info_exclude =
        String::from_utf8(info_exclude).context("Git info/exclude path is not UTF-8")?;
    let info_exclude = Path::new(info_exclude.trim());
    if info_exclude.exists() {
        let bytes = read_git_info_exclude(info_exclude)?;
        controls.push((
            ".git/info/exclude".to_string(),
            format!("{:x}", Sha256::digest(bytes)),
        ));
    }
    controls.sort();
    Ok(controls)
}

fn git_path_is_ignored(root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.excludesFile=/dev/null",
            "check-ignore",
            "--no-index",
            "--quiet",
            "--",
            path,
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(root)
        .output()
        .context("failed to query Git ignore reachability")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "Git ignore reachability query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn read_git_info_exclude(path: &Path) -> Result<Vec<u8>> {
    let leaf = path
        .file_name()
        .context("Git info/exclude path has no leaf")?;
    let parent = path
        .parent()
        .context("Git info/exclude path has no parent")?
        .canonicalize()
        .context("failed to resolve Git info/exclude parent")?;
    let parent = Dir::open_ambient_dir(&parent, ambient_authority())
        .context("failed to capability-open Git info/exclude parent")?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = parent
        .open_with(leaf, &options)
        .context("failed to open Git info/exclude without following symlinks")?;
    let metadata = file
        .metadata()
        .context("failed to inspect opened Git info/exclude")?;
    if !metadata.is_file() {
        bail!("Git info/exclude must be a regular non-symlink file");
    }
    if metadata.len() > MAX_IGNORE_CONTROL_BYTES {
        bail!("Git info/exclude exceeds the source-index control-file bound");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_IGNORE_CONTROL_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read Git info/exclude")?;
    if bytes.len() as u64 > MAX_IGNORE_CONTROL_BYTES {
        bail!("Git info/exclude exceeds the source-index control-file bound");
    }
    Ok(bytes)
}

fn git_output(root: &Path, args: &[&str], disable_system_config: bool) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    if disable_system_config {
        command.env("GIT_CONFIG_NOSYSTEM", "1");
    }
    let output = command
        .output()
        .context("failed to run Git manifest command")?;
    if !output.status.success() {
        bail!(
            "Git manifest command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn normalize_manifest_path(raw: &[u8]) -> Result<String> {
    let path = std::str::from_utf8(raw).context("repository contains a non-UTF-8 path")?;
    if path.is_empty() || path.as_bytes().len() > MAX_PATH_BYTES || path.contains('\\') {
        bail!("repository path is empty, non-normalized, or exceeds {MAX_PATH_BYTES} bytes");
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("Git returned a path outside the repository");
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn extract_agent_lead(source: &ResolvedSource) -> Option<AgentLead> {
    let mut paragraph: Option<(usize, String)> = None;
    for (event, range) in Parser::new(&source.text).into_offset_iter() {
        match event {
            Event::Start(Tag::Paragraph) if paragraph.is_none() => {
                paragraph = Some((range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if paragraph.is_some() => {
                paragraph.as_mut()?.1.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if paragraph.is_some() => {
                paragraph.as_mut()?.1.push(' ');
            }
            Event::End(TagEnd::Paragraph) => {
                let (start_byte, text) = paragraph.take()?;
                let text = bounded_utf8(text.trim(), MAX_AGENT_LEAD_BYTES);
                if text.is_empty() {
                    return None;
                }
                let mut end_byte = range.end.min(start_byte + MAX_AGENT_LEAD_BYTES);
                while !source.text.is_char_boundary(end_byte) {
                    end_byte -= 1;
                }
                let (start_line, end_line) =
                    span_line_coordinates(&source.text, start_byte, end_byte);
                return Some(AgentLead {
                    source: source.identity.clone(),
                    span: SourceSpan {
                        start_byte,
                        end_byte,
                        start_line,
                        end_line,
                    },
                    text,
                });
            }
            _ => {}
        }
    }
    None
}

fn bounded_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn build_directories(
    files: &[IndexedOutline],
    leads: BTreeMap<String, AgentLead>,
) -> Result<Vec<DirectoryRecord>> {
    let mut menus: BTreeMap<String, BTreeMap<String, DirectoryChildKind>> = BTreeMap::new();
    menus.entry(String::new()).or_default();
    for indexed in files {
        let path = Path::new(&indexed.outline.source.path);
        let components = path
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .context("non-UTF-8 indexed path")
            })
            .collect::<Result<Vec<_>>>()?;
        let mut parent = String::new();
        for (index, component) in components.iter().enumerate() {
            let kind = if index + 1 == components.len() {
                DirectoryChildKind::File
            } else {
                DirectoryChildKind::Directory
            };
            menus
                .entry(parent.clone())
                .or_default()
                .entry((*component).to_string())
                .or_insert(kind);
            if kind == DirectoryChildKind::Directory {
                parent = if parent.is_empty() {
                    (*component).to_string()
                } else {
                    format!("{parent}/{component}")
                };
                menus.entry(parent.clone()).or_default();
            }
        }
    }
    if menus.len() > MAX_INDEX_DIRECTORIES {
        bail!("repository has more than {MAX_INDEX_DIRECTORIES} directories");
    }
    let mut directories = Vec::with_capacity(menus.len());
    for (path, children) in menus {
        if children.len() > MAX_DIRECTORY_CHILDREN {
            bail!("directory {path} has more than {MAX_DIRECTORY_CHILDREN} direct children");
        }
        directories.push(DirectoryRecord {
            agent_lead: leads.get(&path).cloned(),
            path,
            children: children
                .into_iter()
                .map(|(name, kind)| DirectoryChild { name, kind })
                .collect(),
        });
    }
    Ok(directories)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ParseCacheKey {
    content_sha256: String,
    parser_selector: String,
    outline_serialization_version: u32,
    outline_algorithm_version: String,
}

impl ParseCacheKey {
    fn for_source(source: &ResolvedSource) -> Self {
        Self::for_identity(&source.identity)
    }

    fn for_identity(identity: &SourceIdentity) -> Self {
        let parser_selector = Path::new(&identity.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        Self {
            content_sha256: identity.content_sha256.clone(),
            parser_selector,
            outline_serialization_version: OUTLINE_SERIALIZATION_VERSION,
            outline_algorithm_version: OUTLINE_ALGORITHM_VERSION.to_string(),
        }
    }

    fn encoded(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}",
            self.content_sha256,
            self.parser_selector,
            self.outline_serialization_version,
            self.outline_algorithm_version
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedOutline {
    provenance: RetrievalProvenance,
    records: Vec<OutlineRecord>,
    truncated: bool,
    parse_had_errors: bool,
}

impl CachedOutline {
    fn from_outline(outline: &OutlineResult) -> Self {
        Self {
            provenance: outline.provenance,
            records: outline.records.clone(),
            truncated: outline.truncated,
            parse_had_errors: outline.parse_had_errors,
        }
    }

    fn with_identity(&self, source: SourceIdentity) -> OutlineResult {
        OutlineResult {
            source,
            provenance: self.provenance,
            records: self.records.clone(),
            truncated: self.truncated,
            parse_had_errors: self.parse_had_errors,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheImage {
    storage_schema_version: u32,
    outline_serialization_version: u32,
    outline_algorithm_version: String,
    snapshot: RepositorySnapshot,
    parse_entries: BTreeMap<String, CachedOutline>,
}

#[derive(Debug, Deserialize)]
struct CacheVersionEnvelope {
    storage_schema_version: Option<u32>,
    outline_serialization_version: Option<u32>,
    outline_algorithm_version: Option<String>,
}

struct PendingBuild {
    snapshot: RepositorySnapshot,
    stats: RepositoryBuildStats,
    parse_entries: BTreeMap<String, CachedOutline>,
}

fn validate_cache_image(image: &CacheImage, expected_namespace: &str) -> IndexResult<()> {
    let snapshot = &image.snapshot;
    if snapshot.workspace_namespace_sha256 != expected_namespace {
        return Err(anyhow::anyhow!(
            "source-index cache workspace namespace does not match its cache slot"
        )
        .into());
    }
    ensure_namespace(&snapshot.workspace_namespace_sha256)?;
    ensure_digest(&snapshot.manifest_sha256, "source-index manifest digest")?;
    if snapshot.repository_revision.is_empty() {
        return Err(anyhow::anyhow!("source-index cache has an empty repository revision").into());
    }
    ensure_file_count(snapshot.files.len())?;
    if snapshot.directories.len() > MAX_INDEX_DIRECTORIES {
        return Err(anyhow::anyhow!(
            "source-index cache exceeds {MAX_INDEX_DIRECTORIES} directories"
        )
        .into());
    }
    if !snapshot
        .directories
        .windows(2)
        .all(|pair| pair[0].path < pair[1].path)
    {
        return Err(
            anyhow::anyhow!("source-index cache directories are not strictly sorted").into(),
        );
    }
    if !snapshot
        .files
        .windows(2)
        .all(|pair| pair[0].outline.source.path.as_str() < pair[1].outline.source.path.as_str())
    {
        return Err(anyhow::anyhow!("source-index cache files are not strictly sorted").into());
    }

    for directory in &snapshot.directories {
        validate_directory_record(directory, expected_namespace, &snapshot.repository_revision)?;
    }
    let mut total_records = 0;
    let mut expected_parse_entries = BTreeMap::new();
    for file in &snapshot.files {
        validate_identity(&file.outline.source, expected_namespace)?;
        if file.outline.source.repository_revision.as_deref()
            != Some(snapshot.repository_revision.as_str())
        {
            return Err(anyhow::anyhow!(
                "source-index cache file identity has a foreign repository revision"
            )
            .into());
        }
        if file.outline.records.len() > MAX_OUTLINE_RECORDS {
            return Err(anyhow::anyhow!(
                "source-index cache file exceeds {MAX_OUTLINE_RECORDS} outline records"
            )
            .into());
        }
        for record in &file.outline.records {
            if record.name.len() > MAX_LABEL_BYTES || record.kind.len() > MAX_LABEL_BYTES {
                return Err(anyhow::anyhow!(
                    "source-index cache outline label or kind exceeds {MAX_LABEL_BYTES} bytes"
                )
                .into());
            }
            validate_span(&record.span, &file.outline.source)?;
        }
        total_records = add_record_count(total_records, file.outline.records.len())?;
        expected_parse_entries.insert(
            ParseCacheKey::for_identity(&file.outline.source).encoded(),
            CachedOutline::from_outline(&file.outline),
        );
    }
    if image.parse_entries != expected_parse_entries {
        return Err(anyhow::anyhow!(
            "source-index cache parse entries do not exactly match the active snapshot"
        )
        .into());
    }
    let leads = snapshot
        .directories
        .iter()
        .filter_map(|directory| {
            directory
                .agent_lead
                .clone()
                .map(|lead| (directory.path.clone(), lead))
        })
        .collect();
    if build_directories(&snapshot.files, leads)? != snapshot.directories {
        return Err(anyhow::anyhow!(
            "source-index cache directory menus do not match the indexed files"
        )
        .into());
    }
    Ok(())
}

fn validate_directory_record(
    directory: &DirectoryRecord,
    expected_namespace: &str,
    expected_revision: &str,
) -> IndexResult<()> {
    if !directory.path.is_empty()
        && normalize_manifest_path(directory.path.as_bytes())? != directory.path
    {
        return Err(anyhow::anyhow!("source-index cache has a non-normalized directory").into());
    }
    if directory.children.len() > MAX_DIRECTORY_CHILDREN {
        return Err(anyhow::anyhow!(
            "source-index cache directory exceeds {MAX_DIRECTORY_CHILDREN} children"
        )
        .into());
    }
    if !directory
        .children
        .windows(2)
        .all(|pair| pair[0].name < pair[1].name)
    {
        return Err(anyhow::anyhow!(
            "source-index cache directory children are not strictly sorted"
        )
        .into());
    }
    for child in &directory.children {
        if child.name.is_empty()
            || child.name.len() > MAX_PATH_BYTES
            || child.name.contains(['/', '\\'])
        {
            return Err(anyhow::anyhow!("source-index cache has an invalid child name").into());
        }
    }
    if let Some(lead) = &directory.agent_lead {
        validate_identity(&lead.source, expected_namespace)?;
        if lead.source.repository_revision.as_deref() != Some(expected_revision) {
            return Err(anyhow::anyhow!(
                "source-index cache AGENTS lead has a foreign repository revision"
            )
            .into());
        }
        let expected_path = if directory.path.is_empty() {
            "AGENTS.md".to_string()
        } else {
            format!("{}/AGENTS.md", directory.path)
        };
        if lead.source.path != expected_path {
            return Err(anyhow::anyhow!(
                "source-index cache AGENTS lead does not belong to its directory"
            )
            .into());
        }
        validate_span(&lead.span, &lead.source)?;
        if lead.text.len() > MAX_AGENT_LEAD_BYTES
            || lead.span.end_byte - lead.span.start_byte > MAX_AGENT_LEAD_BYTES
        {
            return Err(anyhow::anyhow!("source-index cache has an oversized AGENTS lead").into());
        }
    }
    Ok(())
}

fn validate_identity(identity: &SourceIdentity, expected_namespace: &str) -> IndexResult<()> {
    if identity.workspace_namespace_sha256 != expected_namespace {
        return Err(
            anyhow::anyhow!("source-index cache contains a foreign source identity").into(),
        );
    }
    if normalize_manifest_path(identity.path.as_bytes())? != identity.path {
        return Err(
            anyhow::anyhow!("source-index cache contains a non-normalized source path").into(),
        );
    }
    ensure_digest(&identity.content_sha256, "source content digest")?;
    if identity.byte_len as u64 > MAX_SOURCE_BYTES {
        return Err(
            anyhow::anyhow!("source-index cache identity exceeds the source byte bound").into(),
        );
    }
    Ok(())
}

fn validate_span(span: &SourceSpan, identity: &SourceIdentity) -> IndexResult<()> {
    if span.start_byte > span.end_byte
        || span.end_byte > identity.byte_len
        || span.start_line == 0
        || span.end_line < span.start_line
    {
        return Err(anyhow::anyhow!("source-index cache contains an invalid source span").into());
    }
    Ok(())
}

fn ensure_digest(value: &str, label: &str) -> IndexResult<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow::anyhow!("{label} must be 64 hexadecimal characters").into());
    }
    Ok(())
}

fn ensure_namespace(namespace: &str) -> Result<()> {
    if namespace.len() != 64 || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("source-index workspace namespace must be 64 hexadecimal characters");
    }
    Ok(())
}

fn ensure_private_file(file: &std::fs::File, label: &str) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?;
    if !metadata.is_file() {
        bail!("{label} must be a regular file");
    }
    ensure_owner_only_metadata(&metadata, label)
}

fn ensure_owner_only_metadata(metadata: &std::fs::Metadata, label: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
            bail!("{label} is not owned by the current user");
        }
        if metadata.mode() & 0o077 != 0 {
            bail!("{label} must not grant group or other permissions");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Barrier;

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run Git fixture command");
        assert!(
            output.status.success(),
            "Git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn committed_workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("workspace");
        run_git(root.path(), &["init", "-q"]);
        fs::create_dir(root.path().join("src")).expect("src directory");
        fs::write(
            root.path().join("AGENTS.md"),
            "Repository routing guidance.\n\nDo not store this prose body secret.\n",
        )
        .expect("AGENTS fixture");
        fs::write(
            root.path().join("src/lib.rs"),
            "pub fn visible() { let _ = \"PRIVATE_BODY_SENTINEL\"; }\n",
        )
        .expect("Rust fixture");
        fs::write(
            root.path().join("notes.md"),
            "# Public heading\nprivate paragraph\n",
        )
        .expect("Markdown fixture");
        run_git(root.path(), &["add", "."]);
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        root
    }

    fn indexer(root: &Path, state: &Path) -> RepositoryIndexer {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state, fs::Permissions::from_mode(0o700))
                .expect("private state directory");
        }
        let resolver = SourceResolver::new(root).expect("resolver");
        let cache = RepositoryCache::open(state, &resolver).expect("repository cache");
        RepositoryIndexer::new(resolver, cache).expect("repository indexer")
    }

    #[test]
    fn deterministic_build_reuses_parses_and_never_persists_source_bodies() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());

        let first = indexer.build().expect("first build");
        assert_eq!(first.stats.parsed_files, first.stats.indexed_files);
        assert_eq!(first.stats.reused_files, 0);
        assert_eq!(
            first
                .snapshot
                .directory("")
                .and_then(|directory| directory.agent_lead.as_ref())
                .map(|lead| lead.text.as_str()),
            Some("Repository routing guidance.")
        );

        let second = indexer.build().expect("second build");
        assert_eq!(second.snapshot, first.snapshot);
        assert_eq!(second.stats.parsed_files, 0);
        assert_eq!(second.stats.reused_files, second.stats.indexed_files);
        assert!(indexer
            .is_snapshot_current(&second.snapshot)
            .expect("current snapshot"));

        let cache_bytes = fs::read(state.path().join(&indexer.cache.cache_leaf)).expect("cache");
        let cache_text = String::from_utf8(cache_bytes).expect("JSON cache");
        assert!(!cache_text.contains("PRIVATE_BODY_SENTINEL"));
        assert!(!cache_text.contains("private paragraph"));
        assert!(!cache_text.contains("Do not store this prose body secret"));
    }

    #[test]
    fn content_key_reuses_unchanged_files_across_revision_and_reparses_only_changes() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        let initial = indexer.build().expect("initial build");

        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "revision only",
            ],
        );
        let revision_only = indexer.build().expect("revision-only build");
        assert_eq!(revision_only.stats.parsed_files, 0);
        assert_eq!(
            revision_only.stats.reused_files,
            initial.stats.indexed_files
        );

        fs::write(
            root.path().join("notes.md"),
            "# Changed heading\nnew body\n",
        )
        .expect("changed Markdown");
        let changed = indexer.build().expect("changed build");
        assert_eq!(changed.stats.parsed_files, 1);
        assert_eq!(changed.stats.reused_files + 1, changed.stats.indexed_files);
        assert!(!indexer
            .is_snapshot_current(&revision_only.snapshot)
            .expect("stale snapshot"));
    }

    #[test]
    fn git_ignore_controls_and_symlinks_define_the_manifest() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        fs::write(root.path().join(".gitignore"), "ignored.md\n").expect("gitignore");
        fs::write(root.path().join("ignored.md"), "# Must not index\n").expect("ignored file");
        fs::write(root.path().join(".hidden.md"), "# Hidden but indexable\n").expect("hidden file");
        fs::write(root.path().join("info-hidden.md"), "# Info ignored\n")
            .expect("info ignored file");
        fs::write(root.path().join(".git/info/exclude"), "info-hidden.md\n").expect("info exclude");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("notes.md"), root.path().join("linked.md"))
            .expect("source symlink");

        let build = indexer(root.path(), state.path()).build().expect("build");
        assert!(build.snapshot.file(".hidden.md").is_some());
        assert!(build.snapshot.file("ignored.md").is_none());
        assert!(build.snapshot.file("info-hidden.md").is_none());
        #[cfg(unix)]
        assert!(build.snapshot.file("linked.md").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn excluded_untracked_symlinks_do_not_participate_in_generation() {
        use std::os::unix::fs::symlink;

        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        let build = indexer.build().expect("build");

        symlink("notes.md", root.path().join("untracked-link.md")).expect("source symlink");
        assert!(indexer
            .is_snapshot_current(&build.snapshot)
            .expect("symlink addition verdict"));
        fs::remove_file(root.path().join("untracked-link.md")).expect("remove source symlink");
        assert!(indexer
            .is_snapshot_current(&build.snapshot)
            .expect("symlink removal verdict"));
    }

    #[test]
    fn ignored_subtree_controls_do_not_participate_in_generation() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        fs::write(root.path().join(".gitignore"), "vendor/\n").expect("root ignore");
        fs::create_dir(root.path().join("vendor")).expect("ignored directory");
        fs::write(root.path().join("vendor/.gitignore"), "first\n")
            .expect("nonparticipating ignore");
        let indexer = indexer(root.path(), state.path());
        let build = indexer.build().expect("build");

        fs::write(root.path().join("vendor/.gitignore"), "second\n")
            .expect("changed nonparticipating ignore");
        assert!(indexer
            .is_snapshot_current(&build.snapshot)
            .expect("ignored control verdict"));
    }

    #[test]
    fn self_ignored_control_in_reachable_directory_participates_in_generation() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        fs::create_dir(root.path().join("nested")).expect("nested directory");
        fs::write(
            root.path().join("nested/.gitignore"),
            ".gitignore\nonly.txt\n# first\n",
        )
        .expect("self-ignored control");
        fs::write(root.path().join("nested/only.txt"), "ignored\n").expect("ignored sibling");
        let indexer = indexer(root.path(), state.path());
        let build = indexer.build().expect("build");

        fs::write(
            root.path().join("nested/.gitignore"),
            ".gitignore\nonly.txt\n# second\n",
        )
        .expect("changed self-ignored control");
        assert!(!indexer
            .is_snapshot_current(&build.snapshot)
            .expect("control freshness verdict"));
    }

    #[test]
    fn skipped_entry_becoming_text_invalidates_the_snapshot() {
        let root = committed_workspace();
        fs::write(root.path().join("binary.dat"), [0xff, 0xfe, 0xfd]).expect("binary fixture");
        fs::write(
            root.path().join("oversized.txt"),
            vec![b'x'; super::super::identity::MAX_SOURCE_BYTES as usize + 1],
        )
        .expect("oversized fixture");
        run_git(root.path(), &["add", "binary.dat", "oversized.txt"]);
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "binary fixture",
            ],
        );
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        let build = indexer.build().expect("build with skipped binary");
        assert!(build.snapshot.file("binary.dat").is_none());
        assert!(build.snapshot.file("oversized.txt").is_none());

        fs::write(root.path().join("binary.dat"), "now indexable\n").expect("text transition");
        assert!(!indexer
            .is_snapshot_current(&build.snapshot)
            .expect("transition verdict"));

        let rebuilt = indexer.build().expect("binary transition rebuild");
        fs::write(root.path().join("oversized.txt"), "now bounded\n").expect("bounded transition");
        assert!(!indexer
            .is_snapshot_current(&rebuilt.snapshot)
            .expect("oversized transition verdict"));
    }

    #[cfg(unix)]
    #[test]
    fn equal_content_symlink_replacement_fails_freshness_and_span_consumption() {
        use std::os::unix::fs::symlink;

        let root = committed_workspace();
        fs::write(root.path().join("first.md"), "# Same\n").expect("first source");
        fs::write(root.path().join("second.md"), "# Same\n").expect("second source");
        run_git(root.path(), &["add", "first.md", "second.md"]);
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "equal sources",
            ],
        );
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        let build = indexer.build().expect("build");
        let indexed = build.snapshot.file("first.md").expect("first outline");
        let span = indexed.outline.records[0].span.clone();
        let identity = indexed.outline.source.clone();
        fs::remove_file(root.path().join("first.md")).expect("remove first source");
        symlink("second.md", root.path().join("first.md")).expect("equal-content symlink");

        assert!(!indexer
            .is_snapshot_current(&build.snapshot)
            .expect("symlink freshness"));
        let resolver = SourceResolver::new(root.path()).expect("resolver");
        assert!(resolver.read_span(&identity, &span).is_err());
    }

    #[test]
    fn concurrent_builders_publish_one_complete_generation() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700))
                .expect("private state");
        }
        let root_path = root.path().to_path_buf();
        let state_path = state.path().to_path_buf();
        let first = std::thread::spawn({
            let root_path = root_path.clone();
            let state_path = state_path.clone();
            move || {
                indexer(&root_path, &state_path)
                    .build()
                    .expect("first builder")
            }
        });
        let second = std::thread::spawn(move || {
            indexer(&root_path, &state_path)
                .build()
                .expect("second builder")
        });
        let first = first.join().expect("first join");
        let second = second.join().expect("second join");
        assert_eq!(first.snapshot, second.snapshot);
        let reader = indexer(root.path(), state.path());
        assert_eq!(
            reader.cache.load().expect("published cache"),
            Some(first.snapshot)
        );
    }

    #[test]
    fn indexer_rejects_cache_from_another_workspace() {
        let first_root = committed_workspace();
        let second_root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700))
                .expect("private state directory");
        }
        let first_resolver = SourceResolver::new(first_root.path()).expect("first resolver");
        let second_resolver = SourceResolver::new(second_root.path()).expect("second resolver");
        let cache = RepositoryCache::open(state.path(), &first_resolver).expect("first cache");

        assert!(RepositoryIndexer::new(second_resolver, cache).is_err());
    }

    #[test]
    fn post_rename_sync_failure_reports_published_complete_image() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        indexer(root.path(), state.path())
            .build()
            .expect("initial build");
        fs::write(root.path().join("notes.md"), "# Durable candidate\nbody\n")
            .expect("changed source");

        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let mut cache = RepositoryCache::open(state.path(), &resolver).expect("cache");
        cache.fail_directory_sync = true;
        let failing = RepositoryIndexer::new(resolver, cache).expect("indexer");
        assert!(matches!(
            failing.build(),
            Err(RepositoryIndexError::PublishedButNotDurable { .. })
        ));

        let published = indexer(root.path(), state.path())
            .cache
            .load()
            .expect("read published image")
            .expect("published snapshot");
        assert_eq!(
            published.file("notes.md").unwrap().outline.records[0].name,
            "Durable candidate"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directory_traversal_descriptor_is_not_fsync_able_on_linux() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());

        // Mechanism pin for the Linux CI EBADF: the capability descriptor that
        // `open_dir_nofollow` returns is O_PATH, which Linux rejects for fsync
        // while still permitting fstat. Publication must therefore fsync a
        // freshly reopened read-only descriptor, never this handle. If
        // cap-std ever stops opening O_PATH here, this assertion fails and the
        // reopen in `sync_directory` should be re-evaluated, not silently
        // simplified away.
        let traversal = indexer
            .cache
            .directory
            .try_clone()
            .expect("traversal descriptor clone")
            .into_std_file();
        let error = traversal
            .sync_all()
            .expect_err("fsync of the O_PATH traversal descriptor must fail on Linux");
        assert_eq!(
            error.raw_os_error(),
            Some(9),
            "directory traversal descriptor fsync must fail with EBADF (9), got {error:?}"
        );

        // The reopened descriptor publication actually syncs must be a real,
        // fsync-able directory descriptor, and a real publication must still
        // succeed end to end.
        indexer
            .cache
            .sync_directory()
            .expect("reopened descriptor must be fsync-able");
        indexer
            .build()
            .expect("publication with directory sync confirmation");
    }

    #[test]
    fn publish_confirms_durability_through_a_reopened_descriptor() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        indexer.build().expect("initial build");

        // The descriptor publication syncs is reopened beneath the held
        // capability rather than being the traversal handle, and it must stay
        // fsync-able on a cache that already holds published state.
        indexer
            .cache
            .sync_directory()
            .expect("reopened descriptor must stay fsync-able after publication");
    }

    #[test]
    fn failed_generation_validation_preserves_the_last_complete_cache() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let stable_indexer = indexer(root.path(), state.path());
        let stable = stable_indexer.build().expect("stable build");
        let source_path = root.path().join("src/lib.rs");
        let changed = Arc::new(AtomicBool::new(false));
        let changed_for_hook = Arc::clone(&changed);
        let racing_indexer =
            indexer(root.path(), state.path()).with_before_validation_hook(move || {
                if !changed_for_hook.swap(true, Ordering::SeqCst) {
                    fs::write(&source_path, "pub fn raced() {}\n").expect("raced write");
                }
            });

        let error = racing_indexer
            .build()
            .expect_err("mutating generation must not publish");
        assert!(
            error.to_string().contains("source generation changed"),
            "{error:#}"
        );
        assert_eq!(
            stable_indexer.cache.load().expect("preserved cache"),
            Some(stable.snapshot)
        );
    }

    #[test]
    fn incompatible_cache_requires_explicit_rebuild() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        indexer.build().expect("initial build");
        let cache_path = state.path().join(&indexer.cache.cache_leaf);
        fs::write(
            &cache_path,
            br#"{"storage_schema_version":999,"old_payload":"different shape"}"#,
        )
        .expect("incompatible cache");

        assert!(matches!(
            indexer.build(),
            Err(RepositoryIndexError::IncompatibleCache { .. })
        ));
        let rebuilt = indexer.rebuild().expect("explicit rebuild");
        assert_eq!(rebuilt.stats.parsed_files, rebuilt.stats.indexed_files);
        assert_eq!(
            indexer.cache.load().expect("compatible cache"),
            Some(rebuilt.snapshot)
        );
    }

    #[test]
    fn compatible_cache_with_semantically_invalid_payload_is_rejected() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        indexer.build().expect("initial build");
        let cache_path = state.path().join(&indexer.cache.cache_leaf);
        let image = indexer
            .cache
            .read_image()
            .expect("cache read")
            .expect("cache image");
        let mut invalid_children = image.clone();
        let child = DirectoryChild {
            name: "bounded".to_string(),
            kind: DirectoryChildKind::File,
        };
        invalid_children.snapshot.directories[0].children = vec![child; MAX_DIRECTORY_CHILDREN + 1];
        fs::write(
            &cache_path,
            serde_json::to_vec(&invalid_children).expect("encode invalid child image"),
        )
        .expect("write invalid child image");
        assert!(indexer.cache.load().is_err());

        let mut invalid_label = image;
        invalid_label
            .snapshot
            .files
            .iter_mut()
            .find_map(|file| file.outline.records.first_mut())
            .expect("fixture outline record")
            .name = "x".repeat(MAX_LABEL_BYTES + 1);
        fs::write(
            &cache_path,
            serde_json::to_vec(&invalid_label).expect("encode invalid label image"),
        )
        .expect("write invalid label image");
        assert!(indexer.cache.load().is_err());
    }

    #[test]
    fn failed_explicit_rebuild_preserves_incompatible_bytes() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let stable = indexer(root.path(), state.path());
        stable.build().expect("initial build");
        let cache_path = state.path().join(&stable.cache.cache_leaf);
        let incompatible = br#"{"storage_schema_version":999,"old_payload":"preserve me"}"#;
        fs::write(&cache_path, incompatible).expect("incompatible cache");
        let source_path = root.path().join("notes.md");
        let rebuilding =
            indexer(root.path(), state.path()).with_before_validation_hook(move || {
                fs::write(&source_path, "# raced\n").expect("race rebuild");
            });

        assert!(rebuilding.rebuild().is_err());
        assert_eq!(fs::read(cache_path).expect("preserved bytes"), incompatible);
    }

    #[test]
    fn stale_fixed_temporary_leaf_is_replaced_by_a_complete_image() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        fs::write(
            state.path().join(&indexer.cache.temporary_leaf),
            b"interrupted partial bytes",
        )
        .expect("stale temporary leaf");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                state.path().join(&indexer.cache.temporary_leaf),
                fs::Permissions::from_mode(0o600),
            )
            .expect("private temporary leaf");
        }

        let build = indexer.build().expect("recovered build");
        assert_eq!(
            indexer.cache.load().expect("complete cache"),
            Some(build.snapshot)
        );
        assert!(!state.path().join(&indexer.cache.temporary_leaf).exists());
    }

    #[test]
    fn participating_directory_bound_is_enforced_before_control_reads() {
        let root = tempfile::tempdir().expect("workspace");
        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let paths = (0..=MAX_INDEX_DIRECTORIES)
            .map(|index| format!("directory-{index}/file.rs"))
            .collect::<Vec<_>>();
        let error = ignore_control_hashes(&resolver, &paths)
            .expect_err("directory bound must reject manifest");
        assert!(error.to_string().contains("participating directories"));
    }

    #[test]
    fn file_record_and_child_bounds_reject_the_first_excess_item() {
        assert!(ensure_file_count(MAX_INDEX_FILES).is_ok());
        assert!(ensure_file_count(MAX_INDEX_FILES + 1).is_err());
        assert_eq!(
            add_record_count(MAX_INDEX_RECORDS - 1, 1).expect("record boundary"),
            MAX_INDEX_RECORDS
        );
        assert!(add_record_count(MAX_INDEX_RECORDS, 1).is_err());

        let files = (0..=MAX_DIRECTORY_CHILDREN)
            .map(|index| IndexedOutline {
                outline: OutlineResult {
                    source: SourceIdentity {
                        workspace_namespace_sha256: "a".repeat(64),
                        path: format!("file-{index}.rs"),
                        repository_revision: Some("revision".to_string()),
                        content_sha256: format!("{index:064x}"),
                        byte_len: 0,
                    },
                    provenance: RetrievalProvenance {
                        class: super::super::RetrievalProvenanceClass::StructuralParser,
                        method: super::super::RetrievalMethod::TreeSitter,
                    },
                    records: Vec::new(),
                    truncated: false,
                    parse_had_errors: false,
                },
            })
            .collect::<Vec<_>>();
        assert!(build_directories(&files, BTreeMap::new()).is_err());
    }

    #[test]
    fn oversized_cache_is_rejected_before_json_decode() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(root.path(), state.path());
        let cache_path = state.path().join(&indexer.cache.cache_leaf);
        let file = fs::File::create(&cache_path).expect("cache file");
        file.set_len(MAX_CACHE_BYTES + 1)
            .expect("sparse oversized cache");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cache_path, fs::Permissions::from_mode(0o600))
                .expect("private cache");
        }
        assert!(indexer.cache.load().is_err());
    }

    #[test]
    fn paths_and_agent_leads_enforce_hard_utf8_bounds() {
        let too_long = "a".repeat(MAX_PATH_BYTES + 1);
        assert!(normalize_manifest_path(too_long.as_bytes()).is_err());
        let source = ResolvedSource {
            canonical: Path::new("AGENTS.md").to_path_buf(),
            text: format!("{}tail\n", "界".repeat(MAX_AGENT_LEAD_BYTES)),
            identity: SourceIdentity {
                workspace_namespace_sha256: "a".repeat(64),
                path: "AGENTS.md".to_string(),
                repository_revision: Some("revision".to_string()),
                content_sha256: "b".repeat(64),
                byte_len: 0,
            },
        };
        let lead = extract_agent_lead(&source).expect("agent lead");
        assert!(lead.text.len() <= MAX_AGENT_LEAD_BYTES);
        assert!(lead.text.is_char_boundary(lead.text.len()));
        assert!(lead.span.end_byte - lead.span.start_byte <= MAX_AGENT_LEAD_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn cache_rejects_symlink_and_non_private_state() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = tempfile::tempdir().expect("state directory");
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o755))
            .expect("public permissions");
        let workspace = tempfile::tempdir().expect("workspace");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        assert!(RepositoryCache::open(state.path(), &resolver).is_err());

        let parent = tempfile::tempdir().expect("parent");
        let target = parent.path().join("target");
        fs::create_dir(&target).expect("target state");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))
            .expect("private permissions");
        let link = parent.path().join("state-link");
        symlink(&target, &link).expect("state symlink");
        assert!(RepositoryCache::open(&link, &resolver).is_err());

        let private_state = tempfile::tempdir().expect("private state");
        let cache = indexer(workspace.path(), private_state.path()).cache;
        let outside = tempfile::NamedTempFile::new().expect("outside cache target");
        symlink(outside.path(), private_state.path().join(&cache.cache_leaf))
            .expect("cache symlink");
        assert!(cache.load().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_creates_private_state_leaf_and_rejects_workspace_quota_excess() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        let parent = tempfile::tempdir().expect("state parent");
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700))
            .expect("private parent");
        let state_path = parent.path().join("source-index");
        RepositoryCache::prepare(&state_path, &resolver).expect("prepare state");
        assert_eq!(
            fs::metadata(&state_path)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        for index in 0..MAX_WORKSPACE_CACHES {
            let path = state_path.join(format!("source-index-{index:064x}.json"));
            fs::write(&path, []).expect("quota fixture");
        }
        let other_workspace = tempfile::tempdir().expect("other workspace");
        let other = SourceResolver::new(other_workspace.path()).expect("other resolver");
        let error = RepositoryCache::prepare(&state_path, &other)
            .err()
            .expect("ninth cache must be rejected");
        assert!(
            error.to_string().contains("workspace caches"),
            "unexpected quota error: {error:#}"
        );
    }

    #[test]
    fn prepare_rejects_total_state_byte_quota_before_cache_creation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let resolver = SourceResolver::new(workspace.path()).expect("resolver");
        let parent = tempfile::tempdir().expect("state parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700))
                .expect("private parent");
        }
        let state_path = parent.path().join("source-index");
        fs::create_dir(&state_path).expect("state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))
                .expect("private state");
        }
        let oversized = state_path.join(format!("source-index-{}.json", "a".repeat(64)));
        fs::File::create(&oversized)
            .expect("quota image")
            .set_len(MAX_STATE_BYTES + 1)
            .expect("sparse quota image");

        let error = RepositoryCache::prepare(&state_path, &resolver)
            .err()
            .expect("byte quota must be rejected");
        assert!(
            error.to_string().contains("state exceeds"),
            "unexpected quota error: {error:#}"
        );
    }

    #[test]
    fn publication_quota_counts_old_and_temporary_images_at_peak() {
        let workspace = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(workspace.path(), state.path());
        fs::File::create(state.path().join(&indexer.cache.cache_leaf))
            .expect("old cache image")
            .set_len(1_024)
            .expect("old cache length");
        fs::File::create(
            state
                .path()
                .join(format!("source-index-{}.json", "f".repeat(64))),
        )
        .expect("other cache image")
        .set_len(MAX_STATE_BYTES - 1_024)
        .expect("other cache length");

        let error = indexer
            .cache
            .publish_image_under_state_lock(&[0])
            .expect_err("old and temporary images must both count at publication peak");
        assert!(
            error.to_string().contains("publication would exceed"),
            "unexpected quota error: {error:#}"
        );
    }

    #[test]
    fn bounded_build_charges_non_utf8_bytes() {
        let workspace = tempfile::tempdir().expect("workspace");
        run_git(workspace.path(), &["init", "-q"]);
        fs::write(workspace.path().join("binary.dat"), [0xff; 8]).expect("binary fixture");
        run_git(workspace.path(), &["add", "."]);
        run_git(
            workspace.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "binary fixture",
            ],
        );
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(workspace.path(), state.path());

        let error = indexer
            .build_bounded(Instant::now() + std::time::Duration::from_secs(1), 7)
            .expect_err("non-UTF-8 source reads must consume the aggregate byte budget");
        assert!(
            error.to_string().contains("aggregate source bytes"),
            "unexpected budget error: {error:#}"
        );
    }

    #[test]
    fn bounded_cache_read_stops_waiting_for_a_held_lock() {
        let workspace = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let indexer = indexer(workspace.path(), state.path());
        let held = indexer.cache.open_lock().expect("held lock");
        FileExt::lock_exclusive(&held).expect("take held lock");

        let error = indexer
            .cache
            .load_with_deadline(Instant::now() + std::time::Duration::from_millis(20))
            .expect_err("bounded read must not wait indefinitely");
        FileExt::unlock(&held).expect("release held lock");
        assert!(
            error.to_string().contains("deadline expired"),
            "unexpected lock error: {error:#}"
        );
    }

    #[test]
    fn concurrent_workspaces_cannot_exceed_shared_cache_image_quota() {
        let parent = tempfile::tempdir().expect("state parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700))
                .expect("private parent");
        }
        let state = parent.path().join("source-index");
        fs::create_dir(&state).expect("state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("private state");
        }
        for index in 0..MAX_WORKSPACE_CACHES - 1 {
            fs::write(state.join(format!("source-index-{index:064x}.json")), [])
                .expect("existing cache image");
        }

        let first_root = committed_workspace();
        let second_root = committed_workspace();
        let barrier = Arc::new(Barrier::new(2));
        let first = indexer(first_root.path(), &state).with_before_validation_hook({
            let barrier = Arc::clone(&barrier);
            move || {
                barrier.wait();
            }
        });
        let second = indexer(second_root.path(), &state).with_before_validation_hook({
            let barrier = Arc::clone(&barrier);
            move || {
                barrier.wait();
            }
        });
        let first_thread = std::thread::spawn(move || first.build());
        let second_thread = std::thread::spawn(move || second.build());
        let outcomes = [
            first_thread.join().expect("first builder"),
            second_thread.join().expect("second builder"),
        ];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(outcomes.iter().filter(|result| result.is_err()).count(), 1);
        let image_count = fs::read_dir(&state)
            .expect("state entries")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("source-index-") && name.ends_with(".json")
                })
            })
            .count();
        assert_eq!(image_count, MAX_WORKSPACE_CACHES);
    }

    #[test]
    fn bounded_build_fails_on_aggregate_bytes_and_expired_deadline() {
        let root = committed_workspace();
        let state = tempfile::tempdir().expect("state directory");
        let byte_error = indexer(root.path(), state.path())
            .build_bounded(Instant::now() + std::time::Duration::from_secs(5), 1)
            .err()
            .expect("aggregate byte budget");
        assert!(byte_error.to_string().contains("aggregate source bytes"));

        let deadline_error = indexer(root.path(), state.path())
            .build_bounded(Instant::now(), MAX_INDEX_FILES as u64 * MAX_SOURCE_BYTES)
            .err()
            .expect("expired deadline");
        assert!(deadline_error.to_string().contains("deadline expired"));
    }
}
