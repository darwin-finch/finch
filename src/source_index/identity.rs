use anyhow::{bail, Context, Result};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;

const MAX_SOURCE_BYTES: u64 = 1_048_576;

/// Immutable identity for the exact source bytes used to produce a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Opaque identity of the canonical local workspace. Moving a checkout
    /// intentionally invalidates identities produced at its old location.
    pub workspace_namespace_sha256: String,
    /// Workspace-relative source path.
    pub path: String,
    /// Repository HEAD when the workspace is a Git checkout.
    pub repository_revision: Option<String>,
    /// SHA-256 of the exact indexed bytes, including dirty working-tree changes.
    pub content_sha256: String,
    /// Source byte length.
    pub byte_len: usize,
}

/// Resolves source paths against one canonical workspace root.
pub struct SourceResolver {
    workspace_root: PathBuf,
    workspace: Dir,
    workspace_namespace_sha256: String,
    #[cfg(test)]
    before_open: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SourceResolver {
    /// Create a resolver after canonicalising an existing workspace root.
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let workspace_root = workspace_root.as_ref().canonicalize().with_context(|| {
            format!(
                "failed to resolve source workspace {}",
                workspace_root.as_ref().display()
            )
        })?;
        let workspace =
            Dir::open_ambient_dir(&workspace_root, ambient_authority()).with_context(|| {
                format!(
                    "failed to open source workspace {}",
                    workspace_root.display()
                )
            })?;
        let workspace_namespace_sha256 = workspace_namespace(&workspace_root);
        Ok(Self {
            workspace_root,
            workspace,
            workspace_namespace_sha256,
            #[cfg(test)]
            before_open: None,
        })
    }

    /// Resolve and read a UTF-8 source file within the workspace size bound.
    pub(crate) fn read(&self, requested: impl AsRef<Path>) -> Result<ResolvedSource> {
        let requested = requested.as_ref();
        let relative = if requested.is_absolute() {
            let canonical_requested = requested.canonicalize().with_context(|| {
                format!("failed to resolve source path {}", requested.display())
            })?;
            canonical_requested
                .strip_prefix(&self.workspace_root)
                .with_context(|| {
                    format!(
                        "source path {} escapes workspace {}",
                        requested.display(),
                        self.workspace_root.display()
                    )
                })?
                .to_path_buf()
        } else {
            requested.to_path_buf()
        };
        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    bail!(
                        "source path {} escapes workspace {}",
                        requested.display(),
                        self.workspace_root.display()
                    );
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            bail!("source path must name a file");
        }
        #[cfg(test)]
        if let Some(hook) = &self.before_open {
            hook();
        }
        let mut file = self.workspace.open(&normalized).with_context(|| {
            format!(
                "failed to open workspace-contained source path {}",
                requested.display()
            )
        })?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect source path {}", requested.display()))?;
        if !metadata.is_file() {
            bail!("source path is not a file: {}", requested.display());
        }
        if metadata.len() > MAX_SOURCE_BYTES {
            bail!(
                "source file is {} bytes; code_outline limit is {} bytes",
                metadata.len(),
                MAX_SOURCE_BYTES
            );
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read source file {}", requested.display()))?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            bail!(
                "source file is {} bytes; code_outline limit is {} bytes",
                bytes.len(),
                MAX_SOURCE_BYTES
            );
        }
        let text = String::from_utf8(bytes.clone())
            .with_context(|| format!("source file is not UTF-8: {}", requested.display()))?;
        let relative = normalized
            .to_str()
            .context("source path is not valid UTF-8")?
            .replace('\\', "/");
        let identity = SourceIdentity {
            workspace_namespace_sha256: self.workspace_namespace_sha256.clone(),
            path: relative,
            repository_revision: git_head(&self.workspace_root),
            content_sha256: sha256_hex(&bytes),
            byte_len: bytes.len(),
        };
        Ok(ResolvedSource {
            canonical: self.workspace_root.join(&identity.path),
            text,
            identity,
        })
    }

    /// Return true only while the source still resolves inside this workspace
    /// and has the same exact bytes as the recorded identity.
    pub fn is_current(&self, identity: &SourceIdentity) -> Result<bool> {
        let current = self.read(&identity.path)?;
        Ok(current.identity == *identity)
    }

    #[cfg(test)]
    pub(crate) fn with_before_open_hook(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.before_open = Some(Arc::new(hook));
        self
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedSource {
    pub canonical: PathBuf,
    pub text: String,
    pub identity: SourceIdentity,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

fn workspace_namespace(workspace_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"finch-workspace\0");
    hasher.update(workspace_root.as_os_str().as_encoded_bytes());
    format!("{:x}", hasher.finalize())
}

fn git_head(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?;
    let revision = revision.trim();
    (!revision.is_empty()).then(|| revision.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run git fixture command");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }

    #[test]
    fn test_source_identity_changes_when_working_tree_bytes_change() {
        let root = tempfile::tempdir().expect("workspace");
        let path = root.path().join("draft.rs");
        fs::write(&path, "fn first() {}\n").expect("first source");
        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let first = resolver.read("draft.rs").expect("first identity").identity;
        assert!(resolver.is_current(&first).expect("current source"));

        fs::write(&path, "fn second() {}\n").expect("second source");
        assert!(
            !resolver.is_current(&first).expect("stale source verdict"),
            "mutating source bytes must invalidate an earlier retrieval result"
        );
    }

    #[test]
    fn test_source_identity_observes_repository_revision_per_read() {
        let root = tempfile::tempdir().expect("workspace");
        run_git(root.path(), &["init", "-q"]);
        fs::write(root.path().join("same.rs"), "fn same() {}\n").expect("source");
        run_git(root.path(), &["add", "same.rs"]);
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "first",
            ],
        );
        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let first = resolver.read("same.rs").expect("first read").identity;

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
                "second",
            ],
        );

        assert!(
            !resolver.is_current(&first).expect("revision verdict"),
            "changing HEAD must invalidate an identity even when file bytes stay equal"
        );
    }

    #[test]
    fn test_source_resolver_rejects_parent_traversal() {
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("workspace");
        fs::create_dir(&root).expect("workspace");
        fs::write(parent.path().join("outside.rs"), "fn outside() {}\n").expect("outside source");
        let resolver = SourceResolver::new(&root).expect("resolver");

        let error = resolver
            .read("../outside.rs")
            .expect_err("parent traversal must fail");
        assert!(error.to_string().contains("escapes workspace"), "{error:#}");
    }

    #[test]
    fn test_source_resolver_enforces_limit_on_bytes_read() {
        let root = tempfile::tempdir().expect("workspace");
        fs::write(
            root.path().join("large.txt"),
            vec![b'x'; MAX_SOURCE_BYTES as usize + 1],
        )
        .expect("large source");
        let resolver = SourceResolver::new(root.path()).expect("resolver");

        let error = resolver
            .read("large.txt")
            .expect_err("oversized bytes must fail");
        assert!(error.to_string().contains("limit"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn test_source_resolver_rejects_symlink_escape_before_reading() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        fs::write(outside.path(), "do not read").expect("outside contents");
        symlink(outside.path(), root.path().join("escape.rs")).expect("escape symlink");
        let resolver = SourceResolver::new(root.path()).expect("resolver");

        let error = resolver.read("escape.rs").expect_err("escape must fail");
        assert!(
            error.to_string().contains("workspace-contained"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_open_is_race_safe_when_parent_becomes_outward_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("workspace");
        let inside = root.path().join("inside");
        fs::create_dir(&inside).expect("inside directory");
        fs::write(inside.join("target.rs"), "fn inside() {}\n").expect("inside source");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("target.rs"), "fn outside_secret() {}\n")
            .expect("outside source");
        let root_path = root.path().to_path_buf();
        let outside_path = outside.path().to_path_buf();
        let resolver = SourceResolver::new(root.path())
            .expect("resolver")
            .with_before_open_hook(move || {
                fs::rename(root_path.join("inside"), root_path.join("inside-original"))
                    .expect("move original directory");
                symlink(&outside_path, root_path.join("inside")).expect("install outward symlink");
            });

        let error = resolver
            .read("inside/target.rs")
            .expect_err("capability open must reject raced symlink escape");
        assert!(
            error.to_string().contains("workspace-contained"),
            "{error:#}"
        );
    }

    #[test]
    fn test_identical_files_in_different_workspaces_have_distinct_namespaces() {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        fs::write(first.path().join("same.rs"), "fn same() {}\n").expect("first source");
        fs::write(second.path().join("same.rs"), "fn same() {}\n").expect("second source");
        let first_identity = SourceResolver::new(first.path())
            .expect("first resolver")
            .read("same.rs")
            .expect("first read")
            .identity;
        let second_resolver = SourceResolver::new(second.path()).expect("second resolver");
        let second_identity = second_resolver
            .read("same.rs")
            .expect("second read")
            .identity;

        assert_ne!(
            first_identity.workspace_namespace_sha256,
            second_identity.workspace_namespace_sha256
        );
        assert!(!second_resolver
            .is_current(&first_identity)
            .expect("cross-workspace verdict"));
    }

    #[cfg(unix)]
    #[test]
    fn test_non_utf8_workspace_names_have_distinct_namespaces() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let parent = tempfile::tempdir().expect("parent");
        let first = parent
            .path()
            .join(OsString::from_vec(b"workspace-\x80".to_vec()));
        let second = parent
            .path()
            .join(OsString::from_vec(b"workspace-\x81".to_vec()));

        assert_ne!(workspace_namespace(&first), workspace_namespace(&second));
    }
}
