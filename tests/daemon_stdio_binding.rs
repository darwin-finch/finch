//! Production-boundary regression for issue #249.
//!
//! The daemon's fd 1 and fd 2 are opened by the frontend before exec, so they
//! reference an inode rather than the log path. After #240 rotates by rename,
//! anything that bypasses `tracing` — `println!`, panic output, ONNX Runtime's
//! C++ stderr — keeps appending to the archived generation, and once that
//! generation is pruned the descriptors point at an unlinked inode that grows
//! invisibly until the daemon exits.
//!
//! `RotatingLog::bind_process_stdio` makes the daemon own those descriptors and
//! re-point them on every rollover.
//!
//! This is a SINGLE test on purpose. It mutates process-global descriptors, and
//! libtest runs `#[test]` functions in parallel threads within one binary, so a
//! second test here would race: its `SavedStdio::capture()` could save this
//! test's log fd and then "restore" fd 1 to a deleted temporary directory.
//! Everything this file needs to assert is asserted below, in order.
//!
//! Writes go through `io::stdout()` rather than `println!` so they reach the
//! descriptor instead of libtest's capture buffer, which intercepts at the Rust
//! level rather than at the fd.

#![cfg(unix)]

use finch::daemon::{RotatingLog, RotationPolicy};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::{Path, PathBuf};

fn generation(path: &Path, index: usize) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

/// Saves fd 1 and fd 2 and restores them on drop, including during unwind.
struct SavedStdio {
    out: i32,
    err: i32,
}

impl SavedStdio {
    fn capture() -> Self {
        // SAFETY: duplicating this process's own standard descriptors.
        let (out, err) = unsafe {
            (
                nix::libc::dup(nix::libc::STDOUT_FILENO),
                nix::libc::dup(nix::libc::STDERR_FILENO),
            )
        };
        assert!(out >= 0, "failed to save stdout: {}", last_error());
        assert!(err >= 0, "failed to save stderr: {}", last_error());
        Self { out, err }
    }
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

impl Drop for SavedStdio {
    fn drop(&mut self) {
        // SAFETY: restoring descriptors this test saved, then closing the
        // saved copies. Both are valid; `capture` asserted on failure.
        unsafe {
            nix::libc::dup2(self.out, nix::libc::STDOUT_FILENO);
            nix::libc::dup2(self.err, nix::libc::STDERR_FILENO);
            nix::libc::close(self.out);
            nix::libc::close(self.err);
        }
    }
}

/// Device and inode a descriptor currently refers to.
fn identity_of_fd(fd: i32) -> (u64, u64) {
    // SAFETY: the descriptor is owned by this process and is not closed —
    // `into_raw_fd` gives ownership straight back after the metadata call.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata().expect("fstat on bound descriptor");
    let identity = (meta.dev(), meta.ino());
    let _ = file.into_raw_fd();
    identity
}

fn identity_of_path(path: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(path).expect("stat on log path");
    (meta.dev(), meta.ino())
}

#[test]
fn test_bound_stdio_follows_log_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");
    let policy = RotationPolicy {
        max_bytes: 128,
        max_files: 3,
    };
    let mut log = RotatingLog::open(&path, policy).unwrap();

    let restore = SavedStdio::capture();

    // Everything between here and `drop(restore)` runs with fd 2 pointing at a
    // temporary file that is deleted on unwind, so a panic in this window is
    // written into the tempdir and lost — leaving a bare FAILED with no
    // message. Collect outcomes and assert after the descriptors are restored.
    let setup = log.bind_process_stdio();

    // Binding must actually re-point the descriptors at the log, which is what
    // the second test in the earlier draft claimed to check and did not.
    let bound_out = identity_of_fd(nix::libc::STDOUT_FILENO);
    let bound_err = identity_of_fd(nix::libc::STDERR_FILENO);
    let active = identity_of_path(&path);

    // Writes that bypass tracing entirely, as println! and ORT's C++ do.
    std::io::stdout()
        .write_all(b"before-rotation-stdout\n")
        .unwrap();
    std::io::stderr()
        .write_all(b"before-rotation-stderr\n")
        .unwrap();

    // Drive the tracing-side writer until exactly one rollover has happened.
    // Looping a fixed number of times instead would depend on byte arithmetic
    // and, at this policy, would rotate often enough to prune the very
    // generation this test asserts on.
    let mut guard = 0;
    let mut rotated = true;
    while !generation(&path, 1).exists() {
        if log
            .write_all(b"tracing-line-padding-padding-padding\n")
            .is_err()
        {
            rotated = false;
            break;
        }
        guard += 1;
        if guard >= 200 {
            rotated = false;
            break;
        }
    }
    let flushed = log.flush();

    let rebound_out = identity_of_fd(nix::libc::STDOUT_FILENO);
    let after_rotation = identity_of_path(&path);

    std::io::stdout()
        .write_all(b"after-rotation-stdout\n")
        .unwrap();
    std::io::stderr()
        .write_all(b"after-rotation-stderr\n")
        .unwrap();

    drop(restore);

    setup.expect("bind_process_stdio failed");
    flushed.expect("flush failed");
    assert!(rotated, "writer never rotated within the guard");
    assert_eq!(bound_out, active, "fd 1 must refer to the active log");
    assert_eq!(bound_err, active, "fd 2 must refer to the active log");
    assert_ne!(
        rebound_out, bound_out,
        "rotation must re-point fd 1 at the new inode, not leave it on the \
         archived generation"
    );
    assert_eq!(
        rebound_out, after_rotation,
        "fd 1 must refer to the post-rotation active log"
    );

    let active_contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        active_contents.contains("after-rotation-stdout")
            && active_contents.contains("after-rotation-stderr"),
        "stdout/stderr must follow rotation into the new active file; before \
         the fix they stayed pinned to the archived inode. Active file:\n{active_contents}"
    );

    let archived: String = (1..=policy.max_files)
        .map(|index| {
            let g = generation(&path, index);
            if g.exists() {
                std::fs::read_to_string(g).unwrap()
            } else {
                String::new()
            }
        })
        .collect();
    assert!(
        archived.contains("before-rotation-stdout"),
        "the pre-rotation write must be preserved in a generation"
    );
}
