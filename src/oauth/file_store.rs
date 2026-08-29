//! Crash-safe portable OAuth credential persistence.

use super::{validate_reference, OAuthCredentialStore, OAuthTokenRecord, MAX_AUTH_BODY_BYTES};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use uuid::Uuid;
use zeroize::Zeroizing;

/// Private 0700 directory containing atomic 0600 token records.
#[derive(Debug, Clone)]
pub struct FileOAuthCredentialStore {
    root: PathBuf,
}

impl FileOAuthCredentialStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn prepare(&self) -> Result<File> {
        secure_directory::open_or_create_private_directory(&self.root)
    }

    fn record_name(reference: &str) -> Result<String> {
        validate_reference(reference)?;
        Ok(format!(
            "{}.json",
            hex::encode(Sha256::digest(reference.as_bytes()))
        ))
    }

    fn lock(&self, directory: &File) -> Result<File> {
        let file = secure_directory::open_file_at(directory, ".lock", true, false)?;
        file.lock_exclusive()
            .context("Failed to lock OAuth credential store")?;
        Ok(file)
    }

    fn read_locked(&self, directory: &File, reference: &str) -> Result<Option<OAuthTokenRecord>> {
        let name = Self::record_name(reference)?;
        let file = match secure_directory::open_file_at(directory, &name, false, false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        validate_open_record(&file)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take((MAX_AUTH_BODY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_AUTH_BODY_BYTES {
            bail!("stored OAuth credential exceeds the size limit");
        }
        let record = serde_json::from_slice(&bytes)
            .context("stored OAuth credential is malformed; re-authentication is required")?;
        Ok(Some(record))
    }

    fn write_locked(
        &self,
        directory: &File,
        reference: &str,
        record: &OAuthTokenRecord,
    ) -> Result<()> {
        let name = Self::record_name(reference)?;
        let temporary_name = format!(".{name}.{}.tmp", Uuid::new_v4());
        let bytes = Zeroizing::new(serde_json::to_vec(record)?);
        if bytes.len() > MAX_AUTH_BODY_BYTES {
            bail!("OAuth credential record exceeds the size limit");
        }
        let result = (|| -> Result<()> {
            let mut file = secure_directory::open_file_at(directory, &temporary_name, true, true)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            secure_directory::rename_at(directory, &temporary_name, &name)
                .context("Failed to atomically replace OAuth credential")?;
            directory.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = secure_directory::unlink_at(directory, &temporary_name);
        }
        result
    }
}

impl OAuthCredentialStore for FileOAuthCredentialStore {
    fn load(&self, reference: &str) -> Result<Option<OAuthTokenRecord>> {
        let directory = self.prepare()?;
        let lock = self.lock(&directory)?;
        let result = self.read_locked(&directory, reference);
        FileExt::unlock(&lock)?;
        result
    }

    fn compare_and_swap(
        &self,
        reference: &str,
        expected_generation: Option<&str>,
        replacement: &OAuthTokenRecord,
    ) -> Result<()> {
        let directory = self.prepare()?;
        let lock = self.lock(&directory)?;
        let actual = self.read_locked(&directory, reference)?;
        if actual.as_ref().map(|record| record.generation.as_str()) != expected_generation {
            FileExt::unlock(&lock)?;
            bail!("OAuth credential changed during mutation; retry safely");
        }
        self.write_locked(&directory, reference, replacement)?;
        FileExt::unlock(&lock)?;
        Ok(())
    }
}

fn validate_open_record(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("stored OAuth credential is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 {
            bail!("stored OAuth credential has an unsafe hard-link count");
        }
        if metadata.uid() != unsafe { nix::libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!("stored OAuth credential permissions or ownership are unsafe");
        }
    }
    Ok(())
}

#[cfg(unix)]
mod secure_directory {
    use anyhow::{bail, Context, Result};
    use nix::fcntl::{open, openat, renameat, OFlag};
    use nix::sys::stat::{fstat, mkdirat, Mode};
    use nix::unistd::{unlinkat as nix_unlinkat, UnlinkatFlags};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::{Component, Path, PathBuf};

    pub(super) fn open_or_create_private_directory(path: &Path) -> Result<File> {
        if !path.is_absolute() {
            bail!("OAuth credential directory must be absolute");
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
                Component::Normal(name) => Ok(PathBuf::from(name)),
                _ => bail!("OAuth credential directory contains an unsafe component"),
            })
            .collect::<Result<Vec<_>>>()?;
        if components.is_empty() {
            bail!("OAuth credential directory cannot be the filesystem root");
        }
        let root = open(
            "/",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let mut directory = unsafe { File::from_raw_fd(root) };
        validate_ancestor(&directory)?;
        for (index, component) in components.iter().enumerate() {
            let final_component = index + 1 == components.len();
            let next = match open_directory_at(&directory, component) {
                Ok(next) => next,
                Err(nix::errno::Errno::ENOENT) => {
                    mkdirat(
                        Some(directory.as_raw_fd()),
                        component.as_os_str(),
                        Mode::from_bits_truncate(0o700),
                    )?;
                    directory.sync_all()?;
                    open_directory_at(&directory, component)?
                }
                Err(error) => return Err(error.into()),
            };
            if final_component {
                validate_private_root(&next)?;
            } else {
                validate_ancestor(&next)?;
            }
            directory = next;
        }
        Ok(directory)
    }

    fn open_directory_at(parent: &File, name: &Path) -> nix::Result<File> {
        let descriptor = openat(
            Some(parent.as_raw_fd()),
            name.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    fn validate_ancestor(directory: &File) -> Result<()> {
        let status = fstat(directory.as_raw_fd())?;
        let owner = status.st_uid;
        let mode = status.st_mode as u32;
        let effective = unsafe { nix::libc::geteuid() };
        if owner != effective && owner != 0 {
            bail!("OAuth credential directory ancestor has unsafe ownership");
        }
        if mode & 0o022 != 0 && !(owner == 0 && mode & 0o1000 != 0) {
            bail!("OAuth credential directory ancestor has unsafe permissions");
        }
        Ok(())
    }

    fn validate_private_root(directory: &File) -> Result<()> {
        let status = fstat(directory.as_raw_fd())?;
        if status.st_uid != unsafe { nix::libc::geteuid() } || status.st_mode as u32 & 0o077 != 0 {
            bail!("OAuth credential directory ownership or permissions are unsafe");
        }
        Ok(())
    }

    pub(super) fn open_file_at(
        directory: &File,
        name: &str,
        create: bool,
        exclusive: bool,
    ) -> Result<File> {
        if name.contains('/') || name.is_empty() {
            bail!("OAuth credential filename is unsafe");
        }
        let mut flags = OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
        if create {
            flags |= OFlag::O_CREAT;
        }
        if exclusive {
            flags |= OFlag::O_EXCL;
        }
        let descriptor = openat(
            Some(directory.as_raw_fd()),
            name,
            flags,
            Mode::from_bits_truncate(0o600),
        )?;
        let file = unsafe { File::from_raw_fd(descriptor) };
        let status = fstat(file.as_raw_fd())?;
        let kind = nix::sys::stat::SFlag::from_bits_truncate(status.st_mode);
        if !kind.contains(nix::sys::stat::SFlag::S_IFREG) {
            bail!("OAuth credential object is not a regular file");
        }
        Ok(file)
    }

    pub(super) fn rename_at(directory: &File, source: &str, destination: &str) -> Result<()> {
        renameat(
            Some(directory.as_raw_fd()),
            source,
            Some(directory.as_raw_fd()),
            destination,
        )
        .context("descriptor-relative rename failed")?;
        Ok(())
    }

    pub(super) fn unlink_at(directory: &File, name: &str) -> Result<()> {
        nix_unlinkat(
            Some(directory.as_raw_fd()),
            name,
            UnlinkatFlags::NoRemoveDir,
        )?;
        Ok(())
    }
}

#[cfg(not(unix))]
mod secure_directory {
    use anyhow::{bail, Result};
    use std::fs::File;
    use std::path::Path;

    pub(super) fn open_or_create_private_directory(_path: &Path) -> Result<File> {
        bail!("portable OAuth credential persistence is not yet supported on this platform")
    }
    pub(super) fn open_file_at(_: &File, _: &str, _: bool, _: bool) -> Result<File> {
        unreachable!()
    }
    pub(super) fn rename_at(_: &File, _: &str, _: &str) -> Result<()> {
        unreachable!()
    }
    pub(super) fn unlink_at(_: &File, _: &str) -> Result<()> {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, CredentialProvider, EndpointFamily};
    use chrono::{TimeDelta, Utc};
    use std::collections::BTreeSet;

    fn record(generation: &str) -> OAuthTokenRecord {
        OAuthTokenRecord {
            dialect_id: "synthetic".into(),
            protocol_revision: "v1".into(),
            provider: CredentialProvider::ChatgptSubscription,
            kind: CredentialKind::OauthDevice,
            issuer: "issuer".into(),
            audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
            client_id: "client".into(),
            account: "account".into(),
            tenant: None,
            project: None,
            scopes: BTreeSet::from(["read".into()]),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            id_token: Some("id-secret".into()),
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: generation.into(),
            revoked: false,
            mutation_pending: false,
        }
    }

    #[test]
    fn crash_safe_store_reopens_and_rejects_stale_generation_without_secret_debug() {
        let temporary = tempfile::tempdir().unwrap();
        let store = FileOAuthCredentialStore::new(temporary.path().join("oauth"));
        store
            .compare_and_swap("chatgpt:work", None, &record("one"))
            .unwrap();
        let reopened = FileOAuthCredentialStore::new(temporary.path().join("oauth"));
        let loaded = reopened.load("chatgpt:work").unwrap().unwrap();
        assert_eq!(loaded.generation, "one");
        assert!(!format!("{loaded:?}").contains("access-secret"));
        let error = reopened
            .compare_and_swap("chatgpt:work", Some("stale"), &record("two"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed during mutation"));
        assert_eq!(
            reopened.load("chatgpt:work").unwrap().unwrap().generation,
            "one"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_rejects_symlink_and_hardlink_records() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let store = FileOAuthCredentialStore::new(root.clone());
        store
            .compare_and_swap("chatgpt:work", None, &record("one"))
            .unwrap();
        let name = FileOAuthCredentialStore::record_name("chatgpt:work").unwrap();
        let record_path = root.join(&name);
        let hardlink = root.join("hardlink");
        std::fs::hard_link(&record_path, &hardlink).unwrap();
        assert!(store
            .load("chatgpt:work")
            .unwrap_err()
            .to_string()
            .contains("hard-link"));
        std::fs::remove_file(hardlink).unwrap();
        std::fs::remove_file(&record_path).unwrap();
        symlink(temporary.path().join("outside"), &record_path).unwrap();
        assert!(store.load("chatgpt:work").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_bound_write_ignores_ancestor_swap_and_never_touches_external_sentinel() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let moved = temporary.path().join("oauth-bound");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let store = FileOAuthCredentialStore::new(root.clone());
        let directory = store.prepare().unwrap();
        std::fs::rename(&root, &moved).unwrap();
        symlink(&outside, &root).unwrap();
        store
            .write_locked(&directory, "chatgpt:work", &record("one"))
            .unwrap();
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
        assert!(!outside
            .join(FileOAuthCredentialStore::record_name("chatgpt:work").unwrap())
            .exists());
        assert!(moved
            .join(FileOAuthCredentialStore::record_name("chatgpt:work").unwrap())
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_bound_atomic_replace_does_not_follow_swapped_final_component() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let outside = temporary.path().join("sentinel");
        std::fs::write(&outside, b"untouched").unwrap();
        let store = FileOAuthCredentialStore::new(root.clone());
        let directory = store.prepare().unwrap();
        let name = FileOAuthCredentialStore::record_name("chatgpt:work").unwrap();
        symlink(&outside, root.join(&name)).unwrap();
        store
            .write_locked(&directory, "chatgpt:work", &record("one"))
            .unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        let metadata = std::fs::symlink_metadata(root.join(name)).unwrap();
        assert!(metadata.is_file() && !metadata.file_type().is_symlink());
    }
}
