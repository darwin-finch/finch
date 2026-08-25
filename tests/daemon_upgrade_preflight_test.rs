#![cfg(unix)]

use finch::daemon::DaemonUpgradePlan;
use std::os::unix::fs::PermissionsExt;

#[tokio::test(flavor = "multi_thread")]
async fn staged_candidate_boots_on_isolated_http_and_ipc_endpoints() {
    let temp = tempfile::tempdir().unwrap();
    let incumbent = temp.path().join("incumbent");
    std::fs::write(&incumbent, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&incumbent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let empty_brains = temp.path().join("brains");
    std::fs::create_dir(&empty_brains).unwrap();

    let plan = DaemonUpgradePlan::prepare_with_stage_root(
        std::path::Path::new(env!("CARGO_BIN_EXE_finch")),
        &incumbent,
        "none",
        &temp.path().join("stage"),
    )
    .unwrap();
    let candidate = plan.preflight_against(Some(&empty_brains)).await.unwrap();

    assert_eq!(candidate.plan().schema_impact, "none");
    assert!(candidate.plan().candidate.exists());
    assert!(candidate.plan().rollback.exists());
    // Dropping the guard terminates the isolated daemon. No production PID,
    // socket, configuration, credentials, or Brain store was used.
    drop(candidate);
}
