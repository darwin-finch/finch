//! The built `finch` binary executes and reports its compiled version.
//!
//! Replaces the former CI stage that ran `cargo run -- --version`: the bin
//! target is already built by `cargo test --all-targets`, so this adds no
//! compile time, and it holds even when another `src/bin/` target exists
//! (the env var pins the package-named binary unambiguously).

use std::process::Command;

#[test]
fn finch_binary_reports_its_compiled_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--version")
        .output()
        .expect("finch --version should execute the built binary");
    assert!(
        output.status.success(),
        "finch --version exited non-zero: status={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = concat!("finch ", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.contains(expected),
        "finch --version should report the compiled version {expected:?}; stdout={stdout:?}",
    );
}
