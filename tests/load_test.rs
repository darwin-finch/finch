//! Load tests — simulates high-concurrency workloads targeting 100K+ users.
//!
//! Fast tests run with:  cargo test --test load_test
//! All tests (incl slow): cargo test --test load_test -- --include-ignored

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use finch::node::{IsolatedNodeTestState, WorkTracker};
use finch::server::{
    handle_node_info_from_state_directory, handle_node_stats_from_state_directory,
};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn node_test_router(state: &IsolatedNodeTestState) -> Router {
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

async fn oneshot_get(state: &IsolatedNodeTestState, path: &str) -> axum::response::Response {
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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("failed to read body");
    serde_json::from_slice(&bytes).expect("body is not valid JSON")
}

// ---------------------------------------------------------------------------
// Section 2 — Node endpoint throughput
// ---------------------------------------------------------------------------

/// 1000 concurrent GET /v1/node/info requests must all return 200.
#[tokio::test]
#[ignore] // I/O-heavy: reads one disposable node identity 1000 times
async fn test_node_info_throughput_1000_concurrent() {
    const CONCURRENCY: usize = 1000;
    let state = IsolatedNodeTestState::new().expect("create disposable node load-test state");
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let sc = Arc::clone(&success_count);
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let resp = oneshot_get(&state, "/v1/node/info").await;
            if resp.status() == StatusCode::OK {
                sc.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        CONCURRENCY,
        "all {CONCURRENCY} concurrent /v1/node/info requests must return 200"
    );
}

/// 500 concurrent GET /v1/node/stats requests — all 200 with valid JSON shape.
#[tokio::test]
#[ignore] // I/O-heavy: reads disposable work statistics 500 times
async fn test_node_stats_throughput_500_concurrent() {
    const CONCURRENCY: usize = 500;
    let state = IsolatedNodeTestState::new().expect("create disposable node load-test state");
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let sc = Arc::clone(&success_count);
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let resp = oneshot_get(&state, "/v1/node/stats").await;
            if resp.status() == StatusCode::OK {
                let json = body_json(resp).await;
                assert!(
                    json.get("queries_processed").is_some(),
                    "/v1/node/stats response must include queries_processed"
                );
                sc.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        CONCURRENCY,
        "all {CONCURRENCY} concurrent /v1/node/stats requests must return 200"
    );
}

// ---------------------------------------------------------------------------
// Section 3 — WorkTracker atomicity
// ---------------------------------------------------------------------------

/// 1000 concurrent tasks record queries — final counts are exact, no updates lost.
#[tokio::test]
async fn test_work_tracker_atomicity_1000_concurrent() {
    const TASKS: usize = 1000;
    const LATENCY: u64 = 42;

    let tracker = WorkTracker::new();

    let mut handles = Vec::with_capacity(TASKS);
    for i in 0..TASKS {
        let t = Arc::clone(&tracker);
        handles.push(tokio::spawn(async move {
            t.record_query(LATENCY, i % 2 == 0); // even → local, odd → teacher
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    let snap = tracker.snapshot();

    assert_eq!(
        snap.queries_processed, TASKS as u64,
        "no query updates must be lost"
    );
    assert_eq!(
        snap.local_queries + snap.teacher_queries,
        TASKS as u64,
        "local + teacher must equal total"
    );
    assert_eq!(
        snap.local_queries,
        (TASKS / 2) as u64,
        "exactly half must be local"
    );
    assert_eq!(
        snap.teacher_queries,
        (TASKS / 2) as u64,
        "exactly half must be teacher"
    );
    assert_eq!(
        snap.total_latency_ms,
        TASKS as u64 * LATENCY,
        "total latency must be exact sum"
    );
    assert_eq!(
        snap.avg_latency_ms(),
        LATENCY as f64,
        "avg latency must equal per-task latency"
    );
}

/// 500 concurrent tasks with varying latencies and flags — no updates lost.
#[tokio::test]
async fn test_work_tracker_no_lost_updates_mixed() {
    const TASKS: usize = 500;

    let tracker = WorkTracker::new();
    let expected_latency = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(TASKS);
    for i in 0..TASKS {
        let t = Arc::clone(&tracker);
        let el = Arc::clone(&expected_latency);
        handles.push(tokio::spawn(async move {
            let latency = (i as u64 % 100) + 1; // 1..=100 ms
            let used_local = i % 3 != 0; // 2/3 local, 1/3 teacher
            el.fetch_add(latency, Ordering::Relaxed);
            t.record_query(latency, used_local);
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    let snap = tracker.snapshot();
    let exp_latency = expected_latency.load(Ordering::Relaxed);

    assert_eq!(snap.queries_processed, TASKS as u64, "no queries lost");
    assert_eq!(
        snap.local_queries + snap.teacher_queries,
        TASKS as u64,
        "local + teacher == total"
    );
    assert_eq!(
        snap.total_latency_ms, exp_latency,
        "total latency: expected {exp_latency}, got {}",
        snap.total_latency_ms
    );
}

/// Multiple WorkTrackers (simulating multiple worker nodes) all count independently.
#[tokio::test]
async fn test_multiple_trackers_independent() {
    const NODES: usize = 10;
    const QUERIES_PER_NODE: u64 = 100;

    let trackers: Vec<Arc<WorkTracker>> = (0..NODES).map(|_| WorkTracker::new()).collect();

    // Each tracker records QUERIES_PER_NODE queries
    let mut handles = Vec::new();
    for tracker in &trackers {
        let t = Arc::clone(tracker);
        handles.push(tokio::spawn(async move {
            for _ in 0..QUERIES_PER_NODE {
                t.record_query(10, true);
            }
        }));
    }
    for h in handles {
        h.await.expect("task panicked");
    }

    // Each node has exactly QUERIES_PER_NODE — no cross-contamination
    for (i, tracker) in trackers.iter().enumerate() {
        let snap = tracker.snapshot();
        assert_eq!(
            snap.queries_processed, QUERIES_PER_NODE,
            "node {i} must have exactly {QUERIES_PER_NODE} queries, got {}",
            snap.queries_processed
        );
    }

    // Total across all nodes
    let total: u64 = trackers
        .iter()
        .map(|t| t.snapshot().queries_processed)
        .sum();
    assert_eq!(total, NODES as u64 * QUERIES_PER_NODE);
}
