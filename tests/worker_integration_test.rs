// Integration tests for the foreign workload acceptance path:
// HTTP endpoints exposed by every finch worker node.
//
// Strategy
// --------
// The two stateless node handlers (handle_node_info / handle_node_stats)
// are tested WITHOUT a running daemon: we build a minimal Axum Router
// containing only those handlers and drive it with tower::ServiceExt::oneshot().
//
// Stateful HTTP and binary lifecycle coverage lives in
// daemon_integration_test.rs, where each case owns a disposable HOME and a
// kernel-assigned endpoint. This file never discovers or contacts a daemon.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tower::ServiceExt; // provides .oneshot()

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum UserNodeIdSnapshot {
    Missing,
    File {
        contents: Vec<u8>,
        modified: std::time::SystemTime,
        length: u64,
    },
    Symlink(PathBuf),
}

fn user_node_id_path() -> PathBuf {
    dirs::home_dir()
        .expect("the test runner must expose a HOME to snapshot")
        .join(".finch/node_id")
}

fn snapshot_user_node_id(path: &Path) -> UserNodeIdSnapshot {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => UserNodeIdSnapshot::Symlink(
            std::fs::read_link(path).expect("the user node_id symlink must remain inspectable"),
        ),
        Ok(metadata) => {
            assert!(
                metadata.is_file(),
                "user node_id has an unexpected file type"
            );
            UserNodeIdSnapshot::File {
                contents: std::fs::read(path)
                    .expect("the user node_id must remain readable for comparison"),
                modified: metadata
                    .modified()
                    .expect("the user node_id modification time must be readable"),
                length: metadata.len(),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => UserNodeIdSnapshot::Missing,
        Err(error) => panic!("could not snapshot the user node_id: {error}"),
    }
}

struct IsolatedNodeState {
    state: finch::node::IsolatedNodeTestState,
    user_node_id: PathBuf,
    user_before: UserNodeIdSnapshot,
}

impl IsolatedNodeState {
    fn new() -> Self {
        let state =
            finch::node::IsolatedNodeTestState::new().expect("create disposable node state parent");
        let user_node_id = user_node_id_path();
        let user_before = snapshot_user_node_id(&user_node_id);
        Self {
            state,
            user_node_id,
            user_before,
        }
    }

    fn assert_user_node_id_unchanged(&self) {
        assert_eq!(
            snapshot_user_node_id(&self.user_node_id),
            self.user_before,
            "worker integration tests must not mutate the user node_id"
        );
    }
}

/// Build a minimal router containing only the two stateless node handlers.
/// No AgentServer state is required, and there is deliberately no ambient
/// HOME fallback: every caller must provide a disposable state directory.
fn node_test_router(state: &finch::node::IsolatedNodeTestState) -> Router {
    use finch::server::{
        handle_node_info_from_state_directory, handle_node_stats_from_state_directory,
    };
    let info_state = state.clone();
    let stats_state = state.clone();
    Router::new()
        .route(
            "/v1/node/info",
            get(move || handle_node_info_from_state_directory(info_state.clone(), false)),
        )
        .route(
            "/v1/node/stats",
            get(move || handle_node_stats_from_state_directory(stats_state.clone())),
        )
}

/// Convenience wrapper: GET a path on the node_test_router via oneshot.
async fn oneshot_get(
    state: &finch::node::IsolatedNodeTestState,
    path: &str,
) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("failed to build request");

    node_test_router(state)
        .oneshot(req)
        .await
        .expect("oneshot failed")
}

/// Read an Axum response body as a parsed serde_json::Value.
async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("failed to read body");
    serde_json::from_slice(&bytes).expect("response body is not valid JSON")
}

// ---------------------------------------------------------------------------
// Stateless handler tests (no daemon required)
// ---------------------------------------------------------------------------

/// /v1/node/info must return 200 with a JSON object containing
/// identity.id and capabilities.ram_gb.
#[tokio::test]
async fn test_node_info_endpoint_format() {
    let state = IsolatedNodeState::new();
    let resp = oneshot_get(&state.state, "/v1/node/info").await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "/v1/node/info should return 200"
    );

    let json = body_json(resp).await;

    assert!(
        json.get("identity").is_some(),
        "response must have 'identity' key; got: {json}"
    );
    assert!(
        json["identity"].get("id").is_some(),
        "identity must have 'id' field; got: {json}"
    );
    assert!(
        json.get("capabilities").is_some(),
        "response must have 'capabilities' key; got: {json}"
    );
    assert!(
        json["capabilities"].get("ram_gb").is_some(),
        "capabilities must have 'ram_gb' field; got: {json}"
    );
    assert!(state.state.node_id_exists().unwrap());
    state.assert_user_node_id_unchanged();
}

/// /v1/node/stats must return 200 with a JSON object containing
/// the queries_processed counter field.
#[tokio::test]
async fn test_node_stats_endpoint_format() {
    let state = IsolatedNodeState::new();
    let resp = oneshot_get(&state.state, "/v1/node/stats").await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "/v1/node/stats should return 200"
    );

    let json = body_json(resp).await;

    assert!(
        json.get("queries_processed").is_some(),
        "stats must have 'queries_processed' field; got: {json}"
    );
    state.assert_user_node_id_unchanged();
}

/// /v1/node/info must include all required fields for network advertisement:
/// id, name (in identity), ram_gb, os, version (in capabilities).
#[tokio::test]
async fn test_node_info_has_required_fields() {
    let state = IsolatedNodeState::new();
    let resp = oneshot_get(&state.state, "/v1/node/info").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;

    // identity section
    let identity = &json["identity"];
    assert!(
        identity.get("id").and_then(|v| v.as_str()).is_some(),
        "identity.id must be a string; got: {identity}"
    );
    assert!(
        identity.get("name").and_then(|v| v.as_str()).is_some(),
        "identity.name must be a string; got: {identity}"
    );

    // capabilities section
    let caps = &json["capabilities"];
    assert!(
        caps.get("ram_gb").and_then(|v| v.as_u64()).is_some(),
        "capabilities.ram_gb must be a non-negative integer; got: {caps}"
    );
    assert!(
        caps.get("os").and_then(|v| v.as_str()).is_some(),
        "capabilities.os must be a string; got: {caps}"
    );
    assert!(
        caps.get("version").and_then(|v| v.as_str()).is_some(),
        "capabilities.version must be a string; got: {caps}"
    );
    state.assert_user_node_id_unchanged();
}

/// Calling /v1/node/info twice must return the same node id.
/// Node identity is stable inside the fixture's disposable state.
#[tokio::test]
async fn test_node_info_stable_id() {
    let state = IsolatedNodeState::new();
    let resp1 = oneshot_get(&state.state, "/v1/node/info").await;
    let resp2 = oneshot_get(&state.state, "/v1/node/info").await;

    assert_eq!(resp1.status(), StatusCode::OK);
    assert_eq!(resp2.status(), StatusCode::OK);

    let json1 = body_json(resp1).await;
    let json2 = body_json(resp2).await;

    let id1 = json1["identity"]["id"]
        .as_str()
        .expect("id must be a string");
    let id2 = json2["identity"]["id"]
        .as_str()
        .expect("id must be a string");

    assert_eq!(
        id1, id2,
        "node id must be stable across requests (expected same UUID twice)"
    );
    state.assert_user_node_id_unchanged();
}

/// 20 concurrent GET /v1/node/info requests must all succeed.
/// Verifies the handler is race-condition-free (config and file I/O are
/// read-only, but concurrent access still exercises locking paths).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_foreign_requests() {
    const CONCURRENCY: usize = 20;
    let state = IsolatedNodeState::new();
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(CONCURRENCY));

    let handles: Vec<_> = (0..CONCURRENCY)
        .map(|_| {
            let isolated_state = state.state.clone();
            let start = std::sync::Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                // Each task builds its own router — oneshot consumes the service.
                let router = node_test_router(&isolated_state);

                let req = Request::builder()
                    .method("GET")
                    .uri("/v1/node/info")
                    .body(Body::empty())
                    .expect("failed to build request");

                router.oneshot(req).await
            })
        })
        .collect();

    let mut identities = Vec::with_capacity(CONCURRENCY);
    for handle in handles {
        let result = handle.await.expect("task panicked");
        match result {
            Ok(resp) => {
                assert_eq!(resp.status(), StatusCode::OK);
                identities.push(body_json(resp).await["identity"]["id"].clone());
            }
            Err(e) => panic!("oneshot returned error: {e}"),
        }
    }

    assert_eq!(
        identities.len(),
        CONCURRENCY,
        "all {CONCURRENCY} concurrent requests must return 200"
    );
    assert!(
        identities.windows(2).all(|pair| pair[0] == pair[1]),
        "all concurrent requests must observe one stable isolated identity"
    );
    assert!(state.state.node_id_exists().unwrap());
    state.assert_user_node_id_unchanged();
}

#[tokio::test]
async fn test_corrupt_isolated_node_identity_fails_without_user_mutation() {
    let state = IsolatedNodeState::new();
    state
        .state
        .seed_node_id_fixture(b"not valid node identity JSON")
        .unwrap();

    let response = oneshot_get(&state.state, "/v1/node/info").await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state
            .state
            .node_id_fixture_equals(b"not valid node identity JSON")
            .unwrap(),
        true,
        "a corrupt isolated identity must fail closed instead of being replaced"
    );
    state.assert_user_node_id_unchanged();
}

#[tokio::test]
async fn test_node_identity_fifo_fails_without_blocking_or_external_mutation() {
    let state = IsolatedNodeState::new();
    state.state.fifo_node_id_fixture().unwrap();

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        oneshot_get(&state.state, "/v1/node/info"),
    )
    .await
    .expect("nonregular node identity must fail without blocking");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    state.assert_user_node_id_unchanged();
}

#[tokio::test]
async fn test_node_identity_symlinks_fail_before_external_mutation() {
    for existing_target in [true, false] {
        let state = IsolatedNodeState::new();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("external-node-id");
        if existing_target {
            std::fs::write(&target, b"keep external").unwrap();
        }
        state.state.symlink_node_id_fixture(&target).unwrap();

        let response = oneshot_get(&state.state, "/v1/node/info").await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        if existing_target {
            assert_eq!(std::fs::read(&target).unwrap(), b"keep external");
        } else {
            assert!(!target.exists());
        }
        state.assert_user_node_id_unchanged();
    }
}

#[tokio::test]
async fn test_node_identity_hardlink_fails_before_external_mutation() {
    let state = IsolatedNodeState::new();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("external-node-id");
    std::fs::write(&target, b"keep external").unwrap();
    state.state.hardlink_node_id_fixture(&target).unwrap();

    let response = oneshot_get(&state.state, "/v1/node/info").await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(std::fs::read(&target).unwrap(), b"keep external");
    state.assert_user_node_id_unchanged();
}

#[tokio::test]
async fn test_node_identity_root_swap_stays_on_pinned_directory() {
    let state = IsolatedNodeState::new();
    let swap = state.state.swap_root_fixture().unwrap();

    let response = oneshot_get(&state.state, "/v1/node/info").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(swap.pinned_node_id_exists());
    assert!(!swap.replacement_node_id_exists());
    assert!(swap.external_sentinel_unchanged());
    state.assert_user_node_id_unchanged();
}

// ---------------------------------------------------------------------------
// Unit-level sanity checks for the data types (no I/O)
// ---------------------------------------------------------------------------

/// WorkStats default-constructed is all zeros.
#[test]
fn test_work_stats_defaults_are_zero() {
    use finch::node::WorkStats;

    let stats = WorkStats::default();
    assert_eq!(stats.queries_processed, 0);
    assert_eq!(stats.local_queries, 0);
    assert_eq!(stats.teacher_queries, 0);
    assert_eq!(stats.avg_latency_ms(), 0.0);
    assert_eq!(stats.local_pct(), 0.0);
}

/// NodeCapabilities::detect() produces plausible values on the current host.
#[test]
fn test_node_capabilities_detect_plausible() {
    use finch::node::NodeCapabilities;

    let caps = NodeCapabilities::detect(false);
    assert!(caps.ram_gb >= 1, "ram_gb should be at least 1");
    assert!(!caps.version.is_empty(), "version should not be empty");
    assert!(!caps.os.is_empty(), "os should not be empty");
    // 'os' should be a known platform string
    let known = ["macos", "linux", "windows"];
    assert!(
        known.contains(&caps.os.as_str()),
        "os value '{}' is not a recognised platform",
        caps.os
    );
}

/// NodeInfo::summary() returns a non-empty string that includes the short id.
#[test]
fn test_node_info_summary_format() {
    use finch::node::NodeInfo;

    let state = IsolatedNodeState::new();
    let info = state
        .state
        .load_node_info(false)
        .expect("isolated NodeInfo load failed");
    let summary = info.summary();

    assert!(!summary.is_empty(), "summary must not be empty");
    assert!(
        summary.contains(&info.identity.short_id()),
        "summary must include the short node id; got: {summary}"
    );
    assert!(
        summary.contains("RAM"),
        "summary must mention RAM; got: {summary}"
    );
    state.assert_user_node_id_unchanged();
}
