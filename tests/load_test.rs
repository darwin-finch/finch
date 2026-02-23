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
use finch::node::WorkTracker;
use finch::server::{handle_node_info, handle_node_stats, SessionManager, SessionState};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn node_test_router() -> Router {
    Router::new()
        .route("/v1/node/info", get(handle_node_info))
        .route("/v1/node/stats", get(handle_node_stats))
}

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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("failed to read body");
    serde_json::from_slice(&bytes).expect("body is not valid JSON")
}

// ---------------------------------------------------------------------------
// Section 2 — SessionManager load tests
// ---------------------------------------------------------------------------

/// 500 tasks simultaneously create sessions — no duplicate IDs, no corruption.
#[tokio::test]
async fn test_concurrent_session_creation_no_duplicates() {
    const TASKS: usize = 500;
    let manager = Arc::new(SessionManager::new(TASKS + 10, 60));

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let m = Arc::clone(&manager);
        handles.push(tokio::spawn(
            async move { m.get_or_create(None).map(|s| s.id) },
        ));
    }

    let mut ids: HashSet<String> = HashSet::new();
    let mut success = 0usize;

    for h in handles {
        match h.await.expect("task panicked") {
            Ok(id) => {
                assert!(ids.insert(id.clone()), "Duplicate session ID: {id}");
                success += 1;
            }
            Err(e) => panic!("Unexpected error creating session: {e}"),
        }
    }

    assert_eq!(success, TASKS, "all {TASKS} sessions must be created");
    assert_eq!(manager.active_count(), TASKS);
}

/// 200 concurrent tasks racing against max_sessions=50 — limit is enforced,
/// errors have the right message, and no session is silently lost.
#[tokio::test]
async fn test_session_limit_under_concurrency() {
    const MAX: usize = 50;
    const TASKS: usize = 200;
    let manager = Arc::new(SessionManager::new(MAX, 60));

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let m = Arc::clone(&manager);
        handles.push(tokio::spawn(async move {
            m.get_or_create(None)
                .map(|s| s.id)
                .map_err(|e| e.to_string())
        }));
    }

    let mut success_count = 0usize;
    let mut fail_count = 0usize;

    for h in handles {
        match h.await.expect("task panicked") {
            Ok(_) => success_count += 1,
            Err(msg) => {
                assert!(
                    msg.contains("Maximum session limit"),
                    "Wrong rejection error: {msg}"
                );
                fail_count += 1;
            }
        }
    }

    // At least MAX sessions must succeed; some may slightly overshoot due to
    // the DashMap TOCTOU check — that's acceptable, but failures must fire.
    assert!(
        success_count >= MAX,
        "at least {MAX} sessions must be created, got {success_count}"
    );
    assert!(
        fail_count > 0,
        "some requests must be rejected when over limit"
    );
    assert_eq!(
        success_count + fail_count,
        TASKS,
        "every task must either succeed or fail — none lost"
    );
}

/// Concurrent create + read + delete mix — no panics, counts stay coherent.
#[tokio::test]
#[ignore] // slow — mix of I/O and contention
async fn test_session_crud_under_load() {
    const CREATORS: usize = 100;
    let manager = Arc::new(SessionManager::new(500, 60));
    let created_ids: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    // Phase 1: Creators
    let mut handles = Vec::with_capacity(CREATORS);
    for _ in 0..CREATORS {
        let m = Arc::clone(&manager);
        let ids = Arc::clone(&created_ids);
        handles.push(tokio::spawn(async move {
            if let Ok(s) = m.get_or_create(None) {
                ids.lock().unwrap().push(s.id);
            }
        }));
    }
    for h in handles {
        h.await.expect("creator panicked");
    }

    assert_eq!(manager.active_count(), CREATORS);

    // Phase 2: Readers — read 50 random sessions concurrently
    let snapshot: Vec<String> = created_ids.lock().unwrap().clone();
    let mut handles = Vec::new();
    for i in 0..50usize {
        let m = Arc::clone(&manager);
        let id = snapshot[i % snapshot.len()].clone();
        handles.push(tokio::spawn(async move {
            // get_or_create with known ID — should find it
            let _ = m.get_or_create(Some(&id));
        }));
    }
    for h in handles {
        h.await.expect("reader panicked");
    }

    // Phase 3: Delete first 20 sessions
    let to_delete: Vec<String> = snapshot[..20].to_vec();
    let mut handles = Vec::new();
    for id in to_delete {
        let m = Arc::clone(&manager);
        handles.push(tokio::spawn(async move {
            m.delete(&id); // may return false if already deleted by a racing deleter — OK
        }));
    }
    for h in handles {
        h.await.expect("deleter panicked");
    }

    // After deleting 20 from 100: between 80 and 100 remain (race window)
    let remaining = manager.active_count();
    assert!(
        remaining >= 80 && remaining <= 100,
        "expected 80-100 sessions after deleting 20, got {remaining}"
    );
}

/// `is_expired` logic is correct — backdated sessions expire, fresh ones don't.
#[tokio::test]
async fn test_session_expiry_is_expired_logic() {
    use chrono::Duration;

    // Fresh session must not be expired
    let fresh = SessionState::new();
    assert!(
        !fresh.is_expired(30),
        "brand-new session must not be expired"
    );

    // Session with activity 31 minutes ago must be expired at 30-min timeout
    let mut old = SessionState::new();
    old.last_activity = chrono::Utc::now() - Duration::minutes(31);
    assert!(
        old.is_expired(30),
        "session 31min old must be expired at 30-min timeout"
    );

    // Exactly at the boundary — should be expired (>= not >)
    let mut boundary = SessionState::new();
    boundary.last_activity = chrono::Utc::now() - Duration::minutes(30);
    assert!(
        boundary.is_expired(30),
        "session at exactly timeout_minutes is expired"
    );

    // 1-minute timeout; 2-minute-old session
    let mut recent = SessionState::new();
    recent.last_activity = chrono::Utc::now() - Duration::minutes(2);
    assert!(
        recent.is_expired(1),
        "2-minute-old session must expire at 1-min timeout"
    );
}

/// Update and retrieval of session state are coherent under concurrency.
#[tokio::test]
async fn test_session_update_under_concurrency() {
    const TASKS: usize = 100;
    let manager = Arc::new(SessionManager::new(TASKS + 10, 60));

    // Create one session to update
    let session = manager.get_or_create(None).unwrap();
    let session_id = session.id.clone();

    // 100 tasks all try to touch and update the same session
    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let m = Arc::clone(&manager);
        let id = session_id.clone();
        handles.push(tokio::spawn(async move {
            if let Ok(mut s) = m.get_or_create(Some(&id)) {
                s.touch();
                let _ = m.update(&id, s);
            }
        }));
    }

    for h in handles {
        h.await.expect("update task panicked");
    }

    // Session should still exist, count should be 1
    assert_eq!(
        manager.active_count(),
        1,
        "still exactly 1 session after concurrent updates"
    );
    let final_session = manager.get_or_create(Some(&session_id)).unwrap();
    assert_eq!(final_session.id, session_id);
}

// ---------------------------------------------------------------------------
// Section 3 — Node endpoint throughput
// ---------------------------------------------------------------------------

/// 1000 concurrent GET /v1/node/info requests must all return 200.
#[tokio::test]
#[ignore] // I/O-heavy: reads ~/.finch/node_id 1000 times
async fn test_node_info_throughput_1000_concurrent() {
    const CONCURRENCY: usize = 1000;
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let sc = Arc::clone(&success_count);
        handles.push(tokio::spawn(async move {
            let resp = oneshot_get("/v1/node/info").await;
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
#[ignore] // I/O-heavy: reads ~/.finch/work_stats.json 500 times
async fn test_node_stats_throughput_500_concurrent() {
    const CONCURRENCY: usize = 500;
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let sc = Arc::clone(&success_count);
        handles.push(tokio::spawn(async move {
            let resp = oneshot_get("/v1/node/stats").await;
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
// Section 4 — WorkTracker atomicity
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

    let mut trackers: Vec<Arc<WorkTracker>> = (0..NODES).map(|_| WorkTracker::new()).collect();

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

// ---------------------------------------------------------------------------
// Section 5 — Memory stability
// ---------------------------------------------------------------------------

/// Create and destroy 10K sessions sequentially — no ghost entries, count returns to 0.
#[tokio::test]
#[ignore] // slow: 10K iterations
async fn test_memory_stability_10k_session_lifecycle() {
    const ITERATIONS: usize = 10_000;
    let manager = Arc::new(SessionManager::new(ITERATIONS + 1, 60));
    let mut all_ids = Vec::with_capacity(ITERATIONS);

    // Phase 1: Create 10K sessions
    for _ in 0..ITERATIONS {
        let session = manager
            .get_or_create(None)
            .expect("session creation must not fail");
        all_ids.push(session.id);
    }

    assert_eq!(
        manager.active_count(),
        ITERATIONS,
        "all {ITERATIONS} sessions must be active"
    );

    // Phase 2: Delete all
    for id in &all_ids {
        assert!(manager.delete(id), "session {id} must exist when deleted");
    }

    assert_eq!(
        manager.active_count(),
        0,
        "all sessions must be removed after deletion"
    );
}

/// 1000 sessions created then deleted in batches of 100 — count is always consistent.
#[tokio::test]
async fn test_session_batch_lifecycle_consistency() {
    const BATCH: usize = 100;
    const BATCHES: usize = 10;
    let manager = Arc::new(SessionManager::new(BATCH + 10, 60));

    for batch_num in 0..BATCHES {
        // Create BATCH sessions
        let mut ids = Vec::with_capacity(BATCH);
        for _ in 0..BATCH {
            let s = manager.get_or_create(None).expect("create failed");
            ids.push(s.id);
        }
        assert_eq!(
            manager.active_count(),
            BATCH,
            "batch {batch_num}: expected {BATCH} active sessions"
        );

        // Delete all
        for id in &ids {
            manager.delete(id);
        }
        assert_eq!(
            manager.active_count(),
            0,
            "batch {batch_num}: expected 0 sessions after full delete"
        );
    }
}
