//! Atomic file replacement for Finch-owned state under the Finch home.
//!
//! Two kinds of file are replaced here: configuration the user explicitly
//! changed (`config.toml`), and runtime state Finch records for itself
//! (`notice_state.toml`). Both go through the same sequence — write a private
//! same-directory temporary, fsync it, then rename it onto the target — so a
//! save that fails partway leaves the previous bytes intact instead of
//! truncating the file whose replacement it was attempting (#76, "Keep
//! ordinary Finch startup byte-for-byte read-only on user configuration",
//! atomic-write clause for intentional saves).
//!
//! Only intentional writes belong here. Ordinary startup performs no save at
//! all; that is pinned by `tests/startup_is_readonly_on_config.rs`.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

/// Replace `target` with `bytes`, atomically.
///
/// The temporary is a `NamedTempFile` in the target's own directory: private
/// (owner-only) by construction, unique per call, and removed on drop when
/// anything fails before the rename, so no `.tmp` litter survives a failure.
/// Renaming replaces the directory entry, which is atomic and never writes
/// through a symlink; a symlink at the target is refused outright rather than
/// silently replaced, matching the loader's refusal to follow one.
///
/// On unix the replacement keeps the previous file's mode, so a save does not
/// quietly widen or narrow the permissions the user had; a fresh file is
/// owner-only.
pub(crate) fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(target) {
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "{} is a symbolic link; refusing to replace it",
                target.display()
            );
        }
    }

    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create {}", parent.display()))?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create a temporary beside {}", target.display()))?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("Failed to write the temporary for {}", target.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("Failed to flush the temporary for {}", target.display()))?;

    preserve_previous_mode(&mut temporary, target);

    temporary.persist(target).map_err(|error| {
        anyhow::anyhow!(
            "Failed to replace {} atomically: {}",
            target.display(),
            error
        )
    })?;

    sync_parent_directory(parent);
    Ok(())
}

/// Keep the mode the target already had, when there is one.
///
/// Best effort by design: a mode that cannot be read or set must not turn a
/// completed write into a reported failure. A fresh file keeps the temporary's
/// owner-only mode, which is the right default for a file holding API keys.
#[cfg(unix)]
fn preserve_previous_mode(temporary: &mut tempfile::NamedTempFile, target: &Path) {
    if let Ok(metadata) = std::fs::metadata(target) {
        let _ = temporary.as_file().set_permissions(metadata.permissions());
    }
}

#[cfg(not(unix))]
fn preserve_previous_mode(_temporary: &mut tempfile::NamedTempFile, _target: &Path) {}

/// Best-effort parent-directory fsync after the rename.
///
/// The rename is atomic whether or not this runs; the sync is what makes the
/// *new directory entry* durable across a crash or power loss. Where the
/// platform cannot open a directory as a file, nothing is lost beyond that
/// durability, so the failure is deliberately swallowed.
#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode()
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// A replacement that cannot complete leaves the previous bytes intact.
    ///
    /// Denying new entries in the directory stops the temporary from being
    /// created while leaving the existing file writable — so a plain write to
    /// the target would succeed and clobber it, and only a
    /// temporary-then-rename sequence fails before touching anything.
    #[cfg(unix)]
    #[test]
    fn test_a_failing_replacement_preserves_the_previous_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("state.toml");
        std::fs::write(&target, b"previous = true\n").unwrap();
        let before_mtime = std::fs::metadata(&target).unwrap().modified().unwrap();

        set_mode(directory.path(), 0o555);
        let outcome = atomic_write(&target, b"replacement = true\n");
        set_mode(directory.path(), 0o755);

        assert!(
            outcome.is_err(),
            "a replacement that cannot create its temporary must report failure"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"previous = true\n",
            "the previous bytes must survive a failed replacement intact"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().modified().unwrap(),
            before_mtime,
            "and the failed replacement must not move the file's mtime"
        );
    }

    /// A completed replacement leaves only the target behind.
    #[test]
    fn test_a_completed_replacement_leaves_no_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("state.toml");

        atomic_write(&target, b"first\n").unwrap();
        atomic_write(&target, b"second\n").unwrap();

        let entries: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["state.toml"],
            "no temporary may survive a completed replacement; directory held {entries:?}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"second\n");
    }

    /// A symlink at the target is refused, and the link's destination is not
    /// written through.
    #[cfg(unix)]
    #[test]
    fn test_a_symlink_target_is_refused_and_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("elsewhere.toml");
        std::fs::write(&real, b"sentinel\n").unwrap();
        let link = directory.path().join("state.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let outcome = atomic_write(&link, b"replacement\n");

        assert!(
            outcome.is_err(),
            "a symlinked target must be refused, not followed or silently replaced"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must survive"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"sentinel\n",
            "nothing may be written through the link"
        );
    }

    /// Replacing an existing file keeps its mode; a fresh file is private.
    #[cfg(unix)]
    #[test]
    fn test_the_replacement_keeps_the_previous_mode_and_a_fresh_file_is_private() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing.toml");
        std::fs::write(&existing, b"old\n").unwrap();
        set_mode(&existing, 0o644);
        let fresh = directory.path().join("fresh.toml");

        atomic_write(&existing, b"new\n").unwrap();
        atomic_write(&fresh, b"new\n").unwrap();

        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"new\n",
            "the content must be replaced"
        );
        assert_eq!(
            mode_of(&existing) & 0o777,
            0o644,
            "an intentional save must not change the permissions the user had"
        );
        assert_eq!(
            mode_of(&fresh) & 0o077,
            0,
            "a freshly written config (it holds API keys) must be owner-only"
        );
    }
}
