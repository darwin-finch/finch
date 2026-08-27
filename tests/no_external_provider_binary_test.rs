#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn assert_codex_was_not_executed(marker: &std::path::Path, boundary: &str) {
    assert!(
        !marker.exists(),
        "hostile codex executable ran during {boundary}"
    );
}

#[test]
fn test_hostile_codex_on_path_is_never_spawned_by_cli_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let bin_dir = directory.path().join("bin");
    let finch_dir = directory.path().join("home/.finch");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&finch_dir).unwrap();

    let marker = directory.path().join("codex-executed");
    let codex = bin_dir.join("codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\nprintf executed > '{}'\nexit 99\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();

    std::fs::write(
        finch_dir.join("config.toml"),
        r#"[[providers]]
type = "chatgpt_subscription"
credential_ref = "codex-app-server:managed"
model = "gpt-5.6-sol"
name = "legacy"
"#,
    )
    .unwrap();

    let finch = env!("CARGO_BIN_EXE_finch");
    let base = |command: &mut Command| {
        command
            .env("HOME", directory.path().join("home"))
            .env("PATH", &bin_dir)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_HOME")
            .env_remove("FINCH_LIVE_CHATGPT_APP_SERVER");
    };

    let mut request = Command::new(finch);
    base(&mut request);
    let output = request
        .args(["query", "do not execute providers"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Legacy chatgpt_subscription profiles are unsupported"),
        "{stderr}"
    );
    assert_codex_was_not_executed(
        &marker,
        "config load, provider construction, startup, or request",
    );

    let mut auth = Command::new(finch);
    base(&mut auth);
    let output = auth.args(["auth", "status", "chatgpt"]).output().unwrap();
    assert!(!output.status.success());
    assert_codex_was_not_executed(&marker, "removed auth command");

    let mut setup = Command::new(finch);
    base(&mut setup);
    let mut child = setup
        .arg("setup")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert_codex_was_not_executed(&marker, "interactive setup startup");
}
