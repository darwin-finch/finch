// Write tool - create or overwrite files
//
// Returns a summary like Claude Code:
//   Created src/foo.rs (42 lines)
//   Updated src/bar.rs (Added 10 lines, removed 3 lines)

use crate::programs::ExecutionEffect;
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::propose::{
    build_review_artifact, open_review_artifact, parse_proposal_decision, proposal_chat_context,
    reconstruct_reviewed_text, verify_render_is_faithful, ProposalDecision,
};

/// Run `~/.finch/hooks/post-save <file_path>` if that script exists.
/// Fire-and-forget — the hook runs in the background; errors are ignored.
fn run_post_save_hook(file_path: &str) {
    if let Some(hook) = dirs::home_dir().map(|mut p| {
        p.push(".finch/hooks/post-save");
        p
    }) {
        if hook.exists() {
            let _ = std::process::Command::new(&hook).arg(file_path).spawn();
        }
    }
}

enum ReviewTarget {
    Missing(MissingTarget),
    Existing { file: File, original: String },
}

#[cfg(unix)]
struct MissingTarget {
    parent: File,
    parent_path: PathBuf,
    missing: Vec<OsString>,
}

#[cfg(not(unix))]
struct MissingTarget;

#[cfg(unix)]
fn open_directory_at(parent: &File, name: &std::ffi::OsStr) -> nix::Result<File> {
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let descriptor = openat(
        Some(parent.as_raw_fd()),
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    // SAFETY: openat returned a new descriptor owned by this File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn snapshot_missing_target(path: &Path) -> Result<MissingTarget> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use std::os::fd::FromRawFd as _;
    use std::path::Component;

    if !path.is_absolute() {
        anyhow::bail!("Interactive write review requires an absolute target path");
    }
    #[cfg(target_os = "macos")]
    let path = if let Ok(relative) = path.strip_prefix("/tmp") {
        Path::new("/private/tmp").join(relative)
    } else if let Ok(relative) = path.strip_prefix("/var") {
        Path::new("/private/var").join(relative)
    } else {
        path.to_path_buf()
    };
    #[cfg(not(target_os = "macos"))]
    let path = path.to_path_buf();

    let components = path
        .strip_prefix("/")?
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => anyhow::bail!("Write target contains an unsafe path component"),
        })
        .collect::<Result<Vec<_>>>()?;
    let Some((_leaf, parents)) = components.split_last() else {
        anyhow::bail!("Write target cannot be the filesystem root");
    };
    let root = open(
        "/",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    // SAFETY: open returned a new descriptor owned by this File.
    let mut parent = unsafe { File::from_raw_fd(root) };
    let mut parent_path = PathBuf::from("/");
    for (index, component) in parents.iter().enumerate() {
        match open_directory_at(&parent, component) {
            Ok(next) => {
                parent = next;
                parent_path.push(component);
            }
            Err(nix::errno::Errno::ENOENT) => {
                return Ok(MissingTarget {
                    parent,
                    parent_path,
                    missing: components[index..].to_vec(),
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Write target ancestry is not a safe directory at {parent_path:?}")
                })
            }
        }
    }
    Ok(MissingTarget {
        parent,
        parent_path,
        missing: vec![components.last().expect("leaf exists").clone()],
    })
}

#[cfg(not(unix))]
fn snapshot_missing_target(_path: &Path) -> Result<MissingTarget> {
    anyhow::bail!(
        "Interactive write review cannot safely pin a missing target's ancestry on this platform"
    )
}

fn open_review_target(file_path: &str) -> Result<ReviewTarget> {
    match OpenOptions::new().read(true).write(true).open(file_path) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .with_context(|| format!("Failed to read review target: {file_path}"))?;
            let original = String::from_utf8(bytes).map_err(|_| {
                anyhow::anyhow!(
                    "{file_path} is not valid UTF-8 text, so it cannot be shown as a reviewable diff"
                )
            })?;
            Ok(ReviewTarget::Existing { file, original })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ReviewTarget::Missing(
            snapshot_missing_target(Path::new(file_path))?,
        )),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to open review target: {file_path}"))
        }
    }
}

#[cfg(unix)]
fn path_still_names_handle(file_path: &str, handle: &File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let path_metadata = fs::metadata(file_path)
        .with_context(|| format!("Failed to inspect review target path: {file_path}"))?;
    let handle_metadata = handle
        .metadata()
        .with_context(|| format!("Failed to inspect retained review target: {file_path}"))?;
    Ok(
        path_metadata.dev() == handle_metadata.dev()
            && path_metadata.ino() == handle_metadata.ino(),
    )
}

#[cfg(unix)]
fn entry_still_names_handle(parent: &File, name: &std::ffi::OsStr, handle: &File) -> Result<bool> {
    use nix::fcntl::AtFlags;
    use nix::sys::stat::{fstat, fstatat};
    use std::os::fd::AsRawFd as _;

    let entry = fstatat(Some(parent.as_raw_fd()), name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    let retained = fstat(handle.as_raw_fd())?;
    Ok(entry.st_dev == retained.st_dev && entry.st_ino == retained.st_ino)
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn publish_noreplace(
    parent: &File,
    staged_name: &std::ffi::OsStr,
    final_name: &std::ffi::OsStr,
) -> nix::Result<()> {
    use nix::fcntl::{renameat2, RenameFlags};
    use std::os::fd::AsRawFd as _;

    renameat2(
        Some(parent.as_raw_fd()),
        staged_name,
        Some(parent.as_raw_fd()),
        final_name,
        RenameFlags::RENAME_NOREPLACE,
    )
}

#[cfg(target_os = "macos")]
fn publish_noreplace(
    parent: &File,
    staged_name: &std::ffi::OsStr,
    final_name: &std::ffi::OsStr,
) -> nix::Result<()> {
    use nix::errno::Errno;
    use std::ffi::CString;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let staged = CString::new(staged_name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let final_name = CString::new(final_name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    // SAFETY: both C strings live for the call, and `parent` owns a valid open
    // directory descriptor. RENAME_EXCL is Darwin's atomic no-replace flag.
    let result = unsafe {
        nix::libc::renameatx_np(
            parent.as_raw_fd(),
            staged.as_ptr(),
            parent.as_raw_fd(),
            final_name.as_ptr(),
            nix::libc::RENAME_EXCL,
        )
    };
    Errno::result(result).map(drop)
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(all(target_os = "linux", target_env = "gnu"))
))]
fn publish_noreplace(
    _parent: &File,
    _staged_name: &std::ffi::OsStr,
    _final_name: &std::ffi::OsStr,
) -> nix::Result<()> {
    Err(nix::errno::Errno::ENOTSUP)
}

#[cfg(unix)]
fn next_stage_name(kind: &str) -> OsString {
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        ".finch-write-{kind}-{}-{nonce}.tmp",
        std::process::id()
    ))
}

#[cfg(unix)]
fn staged_directory_is_empty(directory: &File) -> Result<bool> {
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};

    let cloned = directory.try_clone()?;
    // SAFETY: try_clone duplicated the descriptor; Dir::from takes ownership of it.
    let mut entries =
        nix::dir::Dir::from(unsafe { std::fs::File::from_raw_fd(cloned.into_raw_fd()) })?;
    for entry in entries.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn unlink_if_still_ours(
    parent: &File,
    name: &std::ffi::OsStr,
    handle: &File,
    directory: bool,
) -> Result<()> {
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::os::fd::AsRawFd as _;

    if !entry_still_names_handle(parent, name, handle)? {
        return Ok(());
    }
    let flags = if directory {
        UnlinkatFlags::RemoveDir
    } else {
        UnlinkatFlags::NoRemoveDir
    };
    unlinkat(Some(parent.as_raw_fd()), name, flags).map_err(Into::into)
}

#[cfg(unix)]
fn publish_error(file_path: &str, error: nix::errno::Errno) -> anyhow::Error {
    if error == nix::errno::Errno::EEXIST {
        anyhow::anyhow!(
            "Write not applied: {file_path} was created while the diff was under review"
        )
    } else if error == nix::errno::Errno::ENOTSUP {
        anyhow::anyhow!(
            "Write not applied: this platform cannot atomically publish a reviewed file without replacing an existing target"
        )
    } else {
        anyhow::anyhow!("Failed to atomically publish reviewed file {file_path}: {error}")
    }
}

#[cfg(unix)]
fn publish_exclusive(
    parent: &File,
    staged: &File,
    staged_name: &std::ffi::OsStr,
    final_name: &std::ffi::OsStr,
    directory: bool,
    file_path: &str,
) -> Result<()> {
    if !entry_still_names_handle(parent, staged_name, staged)? {
        anyhow::bail!(
            "Write not applied: a staged path for {file_path} was replaced while the file was being committed"
        );
    }
    if let Err(error) = publish_noreplace(parent, staged_name, final_name) {
        let _ = unlink_if_still_ours(parent, staged_name, staged, directory);
        return Err(publish_error(file_path, error));
    }
    if !entry_still_names_handle(parent, final_name, staged)? {
        anyhow::bail!(
            "Write not applied: {file_path} no longer names the reviewed inode after exclusive publication"
        );
    }
    Ok(())
}

#[cfg(unix)]
fn stage_missing_write(parent: &File, content: &str) -> Result<(File, OsString)> {
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    for _ in 0..128 {
        let name = next_stage_name("file");
        let descriptor = match openat(
            Some(parent.as_raw_fd()),
            name.as_os_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(descriptor) => descriptor,
            Err(nix::errno::Errno::EEXIST) => continue,
            Err(error) => return Err(error).context("Failed to stage reviewed file"),
        };
        // SAFETY: openat returned a new descriptor owned by this File.
        let mut file = unsafe { File::from_raw_fd(descriptor) };
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        return Ok((file, name));
    }
    anyhow::bail!("Failed to reserve a private staging name for reviewed file")
}

#[cfg(unix)]
fn stage_missing_directory(parent: &File) -> Result<(File, OsString)> {
    use nix::sys::stat::{mkdirat, Mode};
    use std::os::fd::AsRawFd as _;

    for _ in 0..128 {
        let name = next_stage_name("dir");
        match mkdirat(
            Some(parent.as_raw_fd()),
            name.as_os_str(),
            Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) => {
                let created = open_directory_at(parent, name.as_os_str())?;
                if !entry_still_names_handle(parent, name.as_os_str(), &created)?
                    || !staged_directory_is_empty(&created)?
                {
                    let _ = unlink_if_still_ours(parent, name.as_os_str(), &created, true);
                    anyhow::bail!(
                        "Write not applied: a staged directory was replaced while the file was being committed"
                    );
                }
                return Ok((created, name));
            }
            Err(nix::errno::Errno::EEXIST) => continue,
            Err(error) => return Err(error).context("Failed to stage reviewed directory"),
        }
    }
    anyhow::bail!("Failed to reserve a private staging name for reviewed directory")
}

#[cfg(not(unix))]
fn path_still_names_handle(_file_path: &str, _handle: &File) -> Result<bool> {
    anyhow::bail!(
        "Interactive write review cannot safely verify file identity on this platform; the write was not applied"
    )
}

#[cfg(unix)]
fn commit_missing_write(file_path: &str, content: &str, target: MissingTarget) -> Result<()> {
    if !path_still_names_handle(
        target.parent_path.to_string_lossy().as_ref(),
        &target.parent,
    )? {
        anyhow::bail!(
            "Write not applied: an ancestor of {file_path} changed while the diff was under review"
        );
    }
    let Some((leaf, directories)) = target.missing.split_last() else {
        anyhow::bail!("Write not applied: missing target has no filename");
    };
    let mut parent = target.parent;
    for directory in directories {
        let (created, staged_name) = stage_missing_directory(&parent)?;
        publish_exclusive(
            &parent,
            &created,
            staged_name.as_os_str(),
            directory.as_os_str(),
            true,
            file_path,
        )?;
        parent = created;
    }
    let (staged, staged_name) = stage_missing_write(&parent, content)?;
    publish_exclusive(
        &parent,
        &staged,
        staged_name.as_os_str(),
        leaf.as_os_str(),
        false,
        file_path,
    )?;
    if !path_still_names_handle(file_path, &staged)? {
        anyhow::bail!(
            "Write not applied: {file_path} no longer names the reviewed file after exclusive publication"
        );
    }
    parent.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn commit_missing_write(_file_path: &str, _content: &str, _target: MissingTarget) -> Result<()> {
    anyhow::bail!("Interactive write review cannot safely create a missing target on this platform")
}

fn commit_reviewed_write(file_path: &str, content: &str, target: ReviewTarget) -> Result<()> {
    match target {
        ReviewTarget::Missing(target) => commit_missing_write(file_path, content, target)?,
        ReviewTarget::Existing { mut file, original } => {
            if !path_still_names_handle(file_path, &file)? {
                anyhow::bail!(
                    "Write not applied: {file_path} now names a different file than the one reviewed"
                );
            }
            file.seek(std::io::SeekFrom::Start(0))?;
            let mut current = Vec::new();
            file.read_to_end(&mut current)?;
            if current != original.as_bytes() {
                anyhow::bail!(
                    "Write not applied: {file_path} changed while the diff was under review"
                );
            }
            file.seek(std::io::SeekFrom::Start(0))?;
            file.write_all(content.as_bytes())?;
            file.set_len(content.len() as u64)?;
            file.sync_all()?;
        }
    }
    Ok(())
}

fn ensure_review_is_faithful(file_path: &str, original: &str, content: &str) -> Result<()> {
    let shown_path = crate::cli::diff::sanitize_terminal(file_path);
    if shown_path != file_path {
        anyhow::bail!(
            "Cannot open a byte-faithful write review: the target path contains terminal control bytes"
        );
    }
    for (label, text) in [("current file", original), ("proposed file", content)] {
        if text.contains('\0') {
            anyhow::bail!(
                "Cannot open a byte-faithful write review: the {label} contains binary NUL bytes"
            );
        }
        if text.contains('\r') {
            anyhow::bail!(
                "Cannot open a byte-faithful write review: the {label} contains CR/CRLF line endings"
            );
        }
        for (offset, character) in text.char_indices() {
            if character == '\n' || !character.is_control() {
                continue;
            }
            let name = match character {
                '\t' => "TAB".to_string(),
                '\u{1b}' => "ESCAPE".to_string(),
                other => format!("control character U+{:04X}", other as u32),
            };
            anyhow::bail!(
                "Cannot open a byte-faithful write review: the {label} contains {name} at byte {offset}"
            );
        }
    }
    Ok(())
}

async fn review_and_apply_write<F, Fut>(
    file_path: &str,
    content: &str,
    open_editor: F,
) -> Result<String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>>>,
{
    let target = open_review_target(file_path)?;
    let original = match &target {
        ReviewTarget::Missing(_) => String::new(),
        ReviewTarget::Existing { original, .. } => original.clone(),
    };
    ensure_review_is_faithful(file_path, &original, content)?;
    let file_diff = match &target {
        ReviewTarget::Missing(_) => crate::cli::diff::FileDiff::from_created(file_path, content),
        ReviewTarget::Existing { original, .. } => {
            crate::cli::diff::FileDiff::from_texts(file_path, original, content)
        }
    };
    let diff = file_diff.to_unified();
    verify_render_is_faithful("write", &file_diff, &diff, &original, content)?;
    let description = format!("Write {} ({} lines)", file_path, content.lines().count());
    let artifact = build_review_artifact(&description, &diff);
    let Some(returned) = open_editor(artifact.clone()).await? else {
        return Ok("Write aborted by user.".to_string());
    };

    match parse_proposal_decision(&returned) {
        ProposalDecision::Cancel => Ok("Write aborted by user.".to_string()),
        ProposalDecision::Chat { .. } => {
            let context = proposal_chat_context(&returned, &artifact);
            Ok(format!(
                "Write not applied. The user asked for a different change instead of approving:\n{context}"
            ))
        }
        ProposalDecision::Execute { source } => {
            let reconstructed = match reconstruct_reviewed_text(
                "write",
                &file_diff.old_path,
                &file_diff.new_path,
                &original,
                &source,
            ) {
                Ok(text) => text,
                Err(error) => return Ok(error.to_string()),
            };
            let created = matches!(&target, ReviewTarget::Missing(_));
            let result_diff = if created {
                crate::cli::diff::FileDiff::from_created(file_path, &reconstructed).to_unified()
            } else {
                crate::cli::diff::FileDiff::from_texts(file_path, &original, &reconstructed)
                    .to_unified()
            };
            commit_reviewed_write(file_path, &reconstructed, target)?;
            if reconstructed == original {
                Ok(format!(
                    "Write not applied: the reviewed patch leaves {file_path} unchanged."
                ))
            } else {
                Ok(result_diff)
            }
        }
    }
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::WorkspaceWrite
    }

    fn description(&self) -> &str {
        "Write the complete content of a file (creates new or fully overwrites existing). \
         Use for new files or when rewriting most of the content. \
         For small targeted changes to an existing file, use the edit tool instead — \
         it is safer and shows a precise diff."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The complete file content to write"
                }
            }),
            required: vec!["file_path".to_string(), "content".to_string()],
        }
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;
        let content = input["content"]
            .as_str()
            .context("Missing content parameter")?;

        // Interactive: review a plaintext diff, then perform the write here —
        // unless the REPL already granted this call (write:*, AutoAccept, or Yes).
        if super::propose::context_should_open_interactive_review(context).await {
            return review_and_apply_write(file_path, content, |artifact| async move {
                open_review_artifact(&artifact).await
            })
            .await;
        }

        // Non-interactive (tests, daemon): write directly.
        let path = Path::new(file_path);
        let is_new = !path.exists();

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directories for: {}", file_path))?;
            }
        }

        if is_new {
            // New file: just write and return summary
            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;
            run_post_save_hook(file_path);

            Ok(crate::cli::diff::FileDiff::from_created(file_path, content).to_unified())
        } else {
            // Existing file: read original, write new, show stats
            let original = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read existing file: {}", file_path))?;

            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;
            run_post_save_hook(file_path);

            Ok(crate::cli::diff::FileDiff::from_texts(file_path, &original, content).to_unified())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[cfg(unix)]
    #[test]
    fn test_atomic_publish_never_replaces_or_unlinks_concurrent_destination() {
        let directory = tempfile::tempdir().expect("temporary publish directory");
        let parent = File::open(directory.path()).expect("open publish directory");
        let (_staged, staged_name) =
            stage_missing_write(&parent, "reviewed bytes\n").expect("stage reviewed bytes");
        let destination = directory.path().join("target.txt");
        fs::write(&destination, "concurrent actor\n").expect("create concurrent destination");

        let error = publish_noreplace(
            &parent,
            staged_name.as_os_str(),
            std::ffi::OsStr::new("target.txt"),
        )
        .expect_err("no-replace publication must reject a concurrent destination");

        assert_eq!(
            error,
            nix::errno::Errno::EEXIST,
            "atomic no-replace publication returned the wrong diagnostic: {error}"
        );
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "concurrent actor\n",
            "a failed publish must preserve the other actor's destination bytes"
        );
        assert!(
            directory.path().join(&staged_name).exists(),
            "a failed publish must not unlink a pathname another actor could have replaced"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_created_intermediate_identity_rejects_replacement() {
        let directory = tempfile::tempdir().expect("temporary ancestry directory");
        let parent = File::open(directory.path()).expect("open ancestry directory");
        fs::create_dir(directory.path().join("created")).expect("create intermediate");
        let retained = open_directory_at(&parent, std::ffi::OsStr::new("created"))
            .expect("retain created intermediate");
        fs::rename(
            directory.path().join("created"),
            directory.path().join("displaced"),
        )
        .expect("displace retained intermediate");
        fs::create_dir(directory.path().join("created")).expect("install replacement");

        assert!(
            !entry_still_names_handle(&parent, std::ffi::OsStr::new("created"), &retained)
                .expect("compare intermediate identity"),
            "a replacement directory must not satisfy the retained intermediate identity"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_exclusive_publish_refuses_replaced_staged_source() {
        let directory = tempfile::tempdir().expect("temporary staged-source directory");
        let parent = File::open(directory.path()).expect("open staged-source directory");
        let (staged, staged_name) =
            stage_missing_write(&parent, "reviewed bytes\n").expect("stage reviewed bytes");
        let stolen = directory.path().join("stolen.txt");
        fs::rename(directory.path().join(&staged_name), &stolen).expect("displace staged source");
        fs::write(directory.path().join(&staged_name), "attacker\n")
            .expect("install replacement at staged name");
        let destination = directory.path().join("target.txt");

        let error = publish_exclusive(
            &parent,
            &staged,
            staged_name.as_os_str(),
            std::ffi::OsStr::new("target.txt"),
            false,
            destination.to_str().expect("utf-8 destination"),
        )
        .expect_err("replaced staged source must not be published");

        assert!(
            error.to_string().contains("replaced"),
            "exclusive publish must refuse a replaced staged source: {error}"
        );
        assert!(
            !destination.exists(),
            "a replaced staged source must not create the destination"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join(&staged_name)).unwrap(),
            "attacker\n",
            "exclusive publish must not move another actor's replacement file"
        );
        assert_eq!(
            fs::read_to_string(&stolen).unwrap(),
            "reviewed bytes\n",
            "the reviewed inode must remain at the displaced path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_exclusive_directory_publish_refuses_replaced_intermediate() {
        let directory = tempfile::tempdir().expect("temporary intermediate directory");
        let parent = File::open(directory.path()).expect("open intermediate directory");
        let (created, staged_name) =
            stage_missing_directory(&parent).expect("stage reviewed directory");
        let displaced = directory.path().join("displaced");
        fs::rename(directory.path().join(&staged_name), &displaced)
            .expect("displace staged directory");
        fs::create_dir(directory.path().join(&staged_name)).expect("install replacement directory");
        fs::write(
            directory.path().join(&staged_name).join("secret.txt"),
            "keep\n",
        )
        .expect("plant secret in replacement");

        let error = publish_exclusive(
            &parent,
            &created,
            staged_name.as_os_str(),
            std::ffi::OsStr::new("created"),
            true,
            "created/child/target.txt",
        )
        .expect_err("replaced staged directory must not be published");

        assert!(
            error.to_string().contains("replaced"),
            "exclusive directory publish must refuse a replaced intermediate: {error}"
        );
        assert!(
            !directory.path().join("created").exists(),
            "a replaced intermediate must not be renamed onto the final directory name"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join(&staged_name).join("secret.txt")).unwrap(),
            "keep\n",
            "exclusive directory publish must not move another actor's replacement tree"
        );
        assert!(
            !displaced.join("child").exists()
                && !directory.path().join(&staged_name).join("child").exists(),
            "a replaced intermediate must not receive reviewed children"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_staged_directory_is_empty_rejects_planted_entries() {
        let directory = tempfile::tempdir().expect("temporary empty-dir probe");
        let empty = File::open(directory.path()).expect("open empty directory");
        assert!(
            staged_directory_is_empty(&empty).expect("scan empty directory"),
            "a newly created staging directory must be empty"
        );
        fs::write(directory.path().join("secret.txt"), "keep\n")
            .expect("plant an entry in the replacement");
        let planted = File::open(directory.path()).expect("open planted directory");
        assert!(
            !staged_directory_is_empty(&planted).expect("scan planted directory"),
            "a replacement directory that already contains files must not pass the empty-dir guard"
        );
    }

    #[tokio::test]
    async fn test_write_new_file() {
        let tool = WriteTool;
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        // Delete so it looks like a new file
        drop(tmp);

        let input = serde_json::json!({
            "file_path": path,
            "content": "line 1\nline 2\nline 3\n"
        });
        let context = crate::tools::types::ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            skip_interactive_review: false,
        };
        let result = tool.execute(input, &context).await.unwrap();
        let diff = crate::cli::diff::FileDiff::parse(&result).unwrap();
        assert_eq!(diff.display_path(), path);
        assert_eq!(diff.old_path, "/dev/null");
        assert!(diff.is_created());
        assert_eq!((diff.added(), diff.removed()), (3, 0));
    }

    fn reply_with(
        edit: impl Fn(String) -> Option<String> + Send + 'static,
    ) -> impl FnOnce(
        String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
    > {
        move |artifact: String| {
            let answer = edit(artifact);
            Box::pin(async move { Ok(answer) })
        }
    }

    fn seed_review_target(contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("write review temp dir");
        let path = dir.path().join("target.txt");
        fs::write(&path, contents).expect("seed write review target");
        (dir, path.to_string_lossy().into_owned())
    }

    /// Fail-before for #517: editing the saved unified diff with action=execute
    /// must apply the reconstructed saved patch, not the model's planned bytes
    /// and not refuse merely because the body changed.
    #[tokio::test]
    async fn test_review_and_apply_write_uses_saved_diff_not_planned() {
        let (_dir, path) = seed_review_target("alpha\nkeep\n");
        let result = review_and_apply_write(
            &path,
            "planned-bytes\nkeep\n",
            reply_with(|artifact| Some(artifact.replace("+planned-bytes", "+reviewed-edit"))),
        )
        .await
        .expect("interactive write");

        let final_bytes = fs::read_to_string(&path).unwrap();
        assert_eq!(
            final_bytes, "reviewed-edit\nkeep\n",
            "saving an edited review diff with action=execute must apply the saved patch; \
             the independently reconstructed result is reviewed-edit\\nkeep\\n; tool said: {result}"
        );
        assert!(
            !final_bytes.contains("planned-bytes"),
            "planned-only bytes must not appear after a reviewed edit; tool said: {result}"
        );
        assert!(
            result.contains("+reviewed-edit") || final_bytes.contains("reviewed-edit"),
            "the tool result must report the reviewed patch that was applied; tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_review_and_apply_write_cancel_leaves_file_unchanged() {
        let (_dir, path) = seed_review_target("original content\n");
        let result = review_and_apply_write(
            &path,
            "model content\n",
            reply_with(|artifact| {
                Some(artifact.replace("# finch: action=execute", "# finch: action=cancel"))
            }),
        )
        .await
        .expect("cancelled write");

        assert!(
            result.contains("aborted by user"),
            "cancel must explain itself; tool said: {result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original content\n",
            "cancel must leave the target byte-for-byte unchanged; tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_review_and_apply_write_malformed_hunk_does_not_write() {
        let (_dir, path) = seed_review_target("original content\n");
        let result = review_and_apply_write(
            &path,
            "model content\n",
            reply_with(|artifact| Some(format!("{artifact}\n+not-a-complete-hunk\n"))),
        )
        .await
        .expect("malformed write review");

        assert!(
            result.contains("left unchanged")
                && (result.contains("malformed") || result.contains("hunk")),
            "a malformed saved hunk must explain the refusal; tool said: {result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original content\n",
            "a malformed saved hunk must leave the target unchanged; tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_review_and_apply_write_extra_file_is_rejected() {
        let (_dir, path) = seed_review_target("original content\n");
        let result = review_and_apply_write(
            &path,
            "model content\n",
            reply_with(|artifact| {
                Some(format!(
                    "{artifact}--- /dev/null\n+++ b/other.txt\n@@ -0,0 +1,1 @@\n+pwned\n"
                ))
            }),
        )
        .await
        .expect("extra-file write review");

        assert!(
            result.contains("left unchanged") && result.contains("additional file"),
            "an extra file in the saved diff must be rejected; tool said: {result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original content\n",
            "rejecting an extra file must leave the target unchanged; tool said: {result}"
        );
        assert!(
            !path.contains("other.txt"),
            "the extra-file header must never become a write destination"
        );
    }

    #[tokio::test]
    async fn test_review_and_apply_write_path_change_is_rejected() {
        let (_dir, path) = seed_review_target("original content\n");
        let result = review_and_apply_write(
            &path,
            "model content\n",
            reply_with(|artifact| {
                let edited = artifact
                    .lines()
                    .map(|line| {
                        if let Some(rest) = line.strip_prefix("+++ ") {
                            format!("+++ b/etc/passwd{rest}")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(format!("{edited}\n"))
            }),
        )
        .await
        .expect("path-change write review");

        assert!(
            result.contains("left unchanged") && result.contains("target path"),
            "a changed +++ header must be rejected; tool said: {result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original content\n",
            "rejecting a path/header change must leave the target unchanged; tool said: {result}"
        );
    }
}
