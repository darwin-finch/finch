// Test router statistics persistence and corruption detection
//
// This test suite verifies that the threshold router correctly handles:
// 1. Corrupted statistics files (100% failure rate)
// 2. Statistics reset and recovery
// 3. Graceful degradation with bad data

use anyhow::Result;
use finch::models::{ThresholdRouter, QueryCategory};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Test that router can detect and recover from corrupted statistics
#[test]
fn test_router_statistics_corruption_detection() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let stats_path = temp_dir.path().join("threshold_router.json");

    // Create a router with some good statistics
    let mut router = ThresholdRouter::new();
    router.learn_local_attempt("What is Rust?", true);
    router.learn_local_attempt("How do I use lifetimes?", true);
    router.save(&stats_path)?;

    // Verify it saved correctly
    let stats = router.stats();
    assert_eq!(stats.total_local_attempts, 2);
    assert_eq!(stats.total_successes, 2);

    // Now corrupt the statistics with 100% failure rate
    let corrupted_json = serde_json::json!({
        "category_stats": {
            "Other": {
                "local_attempts": 1000000,
                "successes": 0,
                "failures": 1000000,
                "avg_confidence": 0.0
            }
        },
        "total_queries": 1000000,
        "total_local_attempts": 1000000,
        "total_successes": 0,
        "confidence_threshold": 0.95,
        "min_samples": 1,
        "target_forward_rate": 0.05
    });
    fs::write(&stats_path, serde_json::to_string_pretty(&corrupted_json)?)?;

    // Load the corrupted statistics
    let corrupted_router = ThresholdRouter::load(&stats_path)?;
    let corrupted_stats = corrupted_router.stats();

    // Verify the corrupted data was loaded
    assert_eq!(corrupted_stats.total_local_attempts, 1000000);
    assert_eq!(corrupted_stats.total_successes, 0);
    assert_eq!(corrupted_stats.success_rate, 0.0);

    // Key assertion: Router should still try local with fresh queries
    // even with 100% historical failure rate (optimistic default)
    let should_try = corrupted_router.should_try_local("New query pattern");
    assert!(should_try, "Router should try local for queries without category history");

    // But should forward for categories with failure history
    let should_forward = !corrupted_router.should_try_local("What is something?"); // "Other" category
    assert!(should_forward, "Router should forward queries in failed categories");

    Ok(())
}

/// Test that router statistics can be reset
#[test]
fn test_router_statistics_reset() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let stats_path = temp_dir.path().join("threshold_router.json");

    // Create router with bad statistics
    let mut router = ThresholdRouter::new();
    for _ in 0..100 {
        router.learn_local_attempt("Test query", false); // 100 failures
    }
    router.save(&stats_path)?;

    // Verify bad statistics
    let stats = router.stats();
    assert_eq!(stats.total_successes, 0);
    assert_eq!(stats.success_rate, 0.0);

    // Delete the file (simulating reset)
    fs::remove_file(&stats_path)?;

    // Create new router (fresh start)
    let fresh_router = ThresholdRouter::new();
    let fresh_stats = fresh_router.stats();

    // Verify fresh statistics
    assert_eq!(fresh_stats.total_queries, 0);
    assert_eq!(fresh_stats.total_local_attempts, 0);
    assert_eq!(fresh_stats.total_successes, 0);

    // Should try local by default (optimistic)
    assert!(fresh_router.should_try_local("Any query"));

    Ok(())
}

/// Test that router handles missing statistics file gracefully
#[test]
fn test_router_missing_statistics_file() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let stats_path = temp_dir.path().join("nonexistent.json");

    // Try to load from non-existent file
    let result = ThresholdRouter::load(&stats_path);

    // Should return error (file not found)
    assert!(result.is_err());

    // But creating a new router should work
    let router = ThresholdRouter::new();
    assert!(router.should_try_local("Test query"));

    Ok(())
}

/// Test that router learns from mixed success/failure patterns
#[test]
fn test_router_learning_from_mixed_patterns() -> Result<()> {
    let mut router = ThresholdRouter::new();

    // Category 1: High success rate (should prefer local)
    for _ in 0..10 {
        router.learn_local_attempt("What is X?", true);
    }
    router.learn_local_attempt("What is Y?", false);

    // Category 2: Low success rate (should forward)
    for _ in 0..10 {
        router.learn_local_attempt("Debug this error", false);
    }
    router.learn_local_attempt("Debug that error", true);

    // Check that router makes appropriate decisions
    let should_try_definition = router.should_try_local("What is Rust?");
    let should_try_debugging = router.should_try_local("Debug my code");

    // Definition queries should try local (high success)
    assert!(should_try_definition, "High-success categories should try local");

    // Debugging queries should forward (low success)
    assert!(!should_try_debugging, "Low-success categories should forward");

    Ok(())
}

/// Test that router respects min_samples threshold
#[test]
fn test_router_min_samples_threshold() -> Result<()> {
    let mut router = ThresholdRouter::new();

    // Default min_samples is 2
    // With only 1 attempt, should still try local (not enough samples)
    router.learn_local_attempt("What is X?", false);

    let should_try = router.should_try_local("What is Y?");
    assert!(should_try, "Should try local when samples < min_samples");

    // With 2+ attempts and low success, should forward
    router.learn_local_attempt("What is Z?", false);

    let should_forward = !router.should_try_local("What is A?");
    assert!(should_forward, "Should forward when samples >= min_samples and success rate low");

    Ok(())
}
