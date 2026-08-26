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
use tower::ServiceExt; // provides .oneshot()

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal router containing only the two stateless node handlers.
/// No AgentServer state required — the handlers load config independently.
fn node_test_router() -> Router {
    use finch::server::{handle_node_info, handle_node_stats};
    Router::new()
        .route("/v1/node/info", get(handle_node_info))
        .route("/v1/node/stats", get(handle_node_stats))
}

/// Convenience wrapper: GET a path on the node_test_router via oneshot.
async fn oneshot_get(path: &str) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("failed to build request");

    node_test_router()
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
    let resp = oneshot_get("/v1/node/info").await;

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
}

/// /v1/node/stats must return 200 with a JSON object containing
/// the queries_processed counter field.
#[tokio::test]
async fn test_node_stats_endpoint_format() {
    let resp = oneshot_get("/v1/node/stats").await;

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
}

/// /v1/node/info must include all required fields for network advertisement:
/// id, name (in identity), ram_gb, os, version (in capabilities).
#[tokio::test]
async fn test_node_info_has_required_fields() {
    let resp = oneshot_get("/v1/node/info").await;
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
}

/// Calling /v1/node/info twice must return the same node id.
/// Node identity is stable across calls (persisted to ~/.finch/node_id).
#[tokio::test]
async fn test_node_info_stable_id() {
    let resp1 = oneshot_get("/v1/node/info").await;
    let resp2 = oneshot_get("/v1/node/info").await;

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
}

/// 20 concurrent GET /v1/node/info requests must all succeed.
/// Verifies the handler is race-condition-free (config and file I/O are
/// read-only, but concurrent access still exercises locking paths).
#[tokio::test]
async fn test_concurrent_foreign_requests() {
    use finch::server::{handle_node_info, handle_node_stats};

    const CONCURRENCY: usize = 20;

    let handles: Vec<_> = (0..CONCURRENCY)
        .map(|_| {
            tokio::spawn(async move {
                // Each task builds its own router — oneshot consumes the service.
                let router: Router = Router::new()
                    .route("/v1/node/info", get(handle_node_info))
                    .route("/v1/node/stats", get(handle_node_stats));

                let req = Request::builder()
                    .method("GET")
                    .uri("/v1/node/info")
                    .body(Body::empty())
                    .expect("failed to build request");

                router.oneshot(req).await
            })
        })
        .collect();

    let mut success_count = 0usize;
    for handle in handles {
        let result = handle.await.expect("task panicked");
        match result {
            Ok(resp) => {
                if resp.status() == StatusCode::OK {
                    success_count += 1;
                }
            }
            Err(e) => panic!("oneshot returned error: {e}"),
        }
    }

    assert_eq!(
        success_count, CONCURRENCY,
        "all {CONCURRENCY} concurrent requests must return 200"
    );
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

    let info = NodeInfo::load(false).expect("NodeInfo::load failed");
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
}
