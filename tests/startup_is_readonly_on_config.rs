//! Ordinary startup must not rewrite `~/.finch/config.toml` (#76).
//!
//! This lives here, not in a unit test, because the property is about a real
//! home directory. `Config::save()` resolves `dirs::home_dir()`, so a unit
//! test pointing at a temporary path cannot observe production writing to the
//! actual config — and three successive attempts to test this in-crate all
//! passed with the defect restored, because they exercised functions the REPL
//! does not call.
//!
//! An integration test gets its own process, so `HOME` can be set safely.

use std::path::Path;

/// The bytes and mtime of a file, for comparison across a call.
fn fingerprint(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
    (
        std::fs::read(path).expect("config must exist"),
        std::fs::metadata(path)
            .expect("config must exist")
            .modified()
            .expect("mtime"),
    )
}

/// The startup licence-notice decision leaves `config.toml` untouched.
///
/// `claim_notice_showing_now` is what `EventLoop::run` calls — the same
/// function, resolving the same home directory. Restoring the original defect
/// (`cfg.license.notice_suppress_until = ...; cfg.save();`) makes this fail,
/// which is what every earlier version of this test could not do.
#[test]
fn test_the_startup_notice_decision_does_not_rewrite_the_config() {
    let home = tempfile::tempdir().expect("tempdir");
    let finch = home.path().join(".finch");
    std::fs::create_dir_all(&finch).expect("create .finch");

    let config = finch.join("config.toml");
    let original = b"# a comment a serializer round-trip would drop\n\
                     [license]\n\
                     license_type = \"noncommercial\"\n";
    std::fs::write(&config, original).expect("write config");
    let before = fingerprint(&config);

    // Coarse filesystem timestamps would hide a rewrite inside the same tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // SAFETY: not "its own process" — libtest runs the tests in this binary on
    // concurrent threads, and an earlier version of this comment said
    // otherwise. What makes it sound is that this is the only test here that
    // mutates the environment (the other passes HOME via `Command::env`), and
    // std serialises `set_var` against its own readers. The residual risk is a
    // non-std `getenv` on another thread, which is why `set_var` is unsafe at
    // all; with one mutator and one reader of this variable, there is none.
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    let shown = finch::config::claim_notice_showing_now(None, chrono::Local::now().date_naive());
    assert!(shown, "nothing recorded yet, so the notice is due");

    let after = fingerprint(&config);
    assert_eq!(
        after.0, before.0,
        "startup rewrote config.toml -- this is the #76 defect"
    );
    assert_eq!(
        after.1, before.1,
        "startup moved config.toml's mtime; an identical-bytes rewrite still \
         tells every backup and sync tool the file changed"
    );

    assert!(
        finch.join("notice_state.toml").exists(),
        "the record belongs in the state file"
    );
}

/// `finch license remove` clears the recorded notice suppression.
///
/// This drives the real binary, because the wiring is what was broken and a
/// helper test could not see it. Before the record moved out of `config.toml`,
/// removal un-suppressed the notice for free — `LicenseRemove` writes
/// `notice_suppress_until: None`. Afterwards the state file won and that
/// stopped working, silently.
///
/// Review of #329 deleted both production call sites and 98 tests still
/// passed; the only thing that had ever noticed one of them was rustc, when it
/// failed to compile. This runs the command a user runs.
#[test]
fn test_license_remove_clears_the_recorded_suppression() {
    let home = tempfile::tempdir().expect("tempdir");
    let finch = home.path().join(".finch");
    std::fs::create_dir_all(&finch).expect("create .finch");

    let state = finch.join("notice_state.toml");
    std::fs::write(&state, "suppress_until = \"2030-01-01\"\n").expect("seed state");
    assert!(state.exists());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["license", "remove"])
        .env("HOME", home.path())
        .output()
        .expect("run finch license remove");

    assert!(
        output.status.success(),
        "`finch license remove` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !state.exists(),
        "removing a licence must clear the recorded suppression, or the notice \
         stays hidden until the old date expires"
    );
}
