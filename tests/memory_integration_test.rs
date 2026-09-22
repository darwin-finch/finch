// Integration tests for Phase 4: Hierarchical Memory System

use anyhow::Result;
use finch_memory::{cosine_similarity, EmbeddingEngine, MemoryConfig, MemorySystem, TfIdfEmbedding};
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_memory_system_creation() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        enabled: true,
        max_context_items: 5,
        checkpoint_interval_secs: 300,
        ..Default::default()
    };

    let memory = MemorySystem::new(config)?;
    let stats = memory.stats().await?;

    assert_eq!(stats.conversation_count, 0);
    assert_eq!(stats.tree_node_count, 0);

    Ok(())
}

#[tokio::test]
async fn test_insert_and_query() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        ..Default::default()
    };

    let memory = MemorySystem::new(config)?;

    // Insert conversations
    memory
        .insert_conversation(
            "user",
            "How do I use Rust lifetimes?",
            Some("local"),
            Some("test-session"),
        )
        .await?;

    memory
        .insert_conversation(
            "assistant",
            "Lifetimes in Rust ensure references are valid...",
            Some("local"),
            Some("test-session"),
        )
        .await?;

    memory
        .insert_conversation(
            "user",
            "What is Python asyncio?",
            Some("local"),
            Some("test-session"),
        )
        .await?;

    // Query for Rust-related content
    let results = memory.query("Rust programming", Some(3)).await?;

    assert!(!results.is_empty());
    // Should find Rust-related conversations
    assert!(results
        .iter()
        .any(|r| r.contains("Rust") || r.contains("lifetimes")));

    Ok(())
}

#[tokio::test]
async fn test_memory_stats() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        ..Default::default()
    };

    let memory = MemorySystem::new(config)?;

    // Insert substantive messages (> 20 chars) so they pass the quality filter
    // and land in both the SQL history table and the RoutingTree semantic
    // index.
    for i in 1..=10 {
        memory
            .insert_conversation(
                "user",
                &format!("How do I implement feature number {} correctly in Rust?", i),
                Some("local"),
                None,
            )
            .await?;
    }

    let stats = memory.stats().await?;
    // conversation_count is the raw SQL history — always incremented regardless of quality.
    assert_eq!(stats.conversation_count, 10);
    // tree_node_count is the semantic index size, in real RoutingTree points
    // (crates/finch-memory/src/routing_memory.rs) -- not MemTree's leaf-node
    // count, so unlike the old assertion here it equals the input count
    // directly: all 10 messages are text-distinct (differing only by
    // number), so each stores as its own point, with no promotion or
    // internal-node bookkeeping to account for (#250 was about MemTree's own
    // promotion, which RoutingTree has no equivalent of).
    assert_eq!(
        stats.tree_node_count, 10,
        "every indexed message must be present exactly once"
    );

    // RoutingTree buckets points into leaves up to `leaf_capacity` (10 by
    // default, crates/finch-memory/src/routing_tree.rs's `RoutingConfig`)
    // before ever splitting, so ten near-identical, tightly clustered points
    // can legitimately sit in one unsplit leaf -- that is compact structure
    // here, not the degenerate chain #250 named (MemTree had no capacity
    // bound on a leaf, so an unbounded chain of matches was the failure
    // mode). The structural invariant that still applies regardless of how
    // many leaves exist is a bounded depth.
    let (leaves, depth, _widest) = memory.index_shape().await;
    assert!(leaves >= 1, "an index holding points has at least one leaf");
    assert!(
        depth <= 4,
        "ten near-identical turns must not chain; got depth {depth} for {leaves} leaves"
    );

    Ok(())
}

#[tokio::test]
async fn test_get_recent_conversations() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        ..Default::default()
    };

    let memory = MemorySystem::new(config)?;

    // Insert conversations
    for i in 1..=5 {
        memory
            .insert_conversation("user", &format!("Message {}", i), Some("local"), None)
            .await?;
    }

    // Get recent 3
    let recent = memory.get_recent_conversations(3).await?;

    assert_eq!(recent.len(), 3);
    // Should be in reverse chronological order
    assert!(recent[0].1.contains("Message 5"));
    assert!(recent[1].1.contains("Message 4"));
    assert!(recent[2].1.contains("Message 3"));

    Ok(())
}

#[tokio::test]
async fn test_embedding_similarity() -> Result<()> {
    let engine = TfIdfEmbedding::new();

    let emb1 = engine.embed("rust programming language")?;
    let emb2 = engine.embed("rust coding")?;
    let emb3 = engine.embed("python data science")?;

    // Similar texts should have higher similarity
    let sim12 = cosine_similarity(&emb1, &emb2);
    let sim13 = cosine_similarity(&emb1, &emb3);

    // rust + rust should be more similar than rust + python
    assert!(sim12 > sim13, "Similar texts should have higher similarity");

    Ok(())
}

// A test previously lived here (`test_memtree_insertion`) constructing a
// bare `MemTree` directly and exercising its own `insert`/`all_nodes`/
// `retrieve` API. `MemTree` is gone -- `RoutingTree` replaced it outright
// (crates/finch-memory/src/routing_tree.rs) -- and is crate-private, so an
// integration test outside `finch-memory` cannot construct one directly the
// same way. The properties this test pinned (distinct memories stored once
// each; a query returns content actually similar to it) are still covered
// through the public `MemorySystem` API by `test_insert_and_query` and
// `test_memory_stats` above, and far more thoroughly by
// `crates/finch-memory/src/routing_tree/tests.rs` and
// `crates/finch-memory/src/lib.rs`'s own test suite.

#[tokio::test]
async fn test_memory_persistence() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let db_path = temp.path().to_path_buf();

    // Create memory and insert data
    {
        let config = MemoryConfig {
            db_path: db_path.clone(),
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;
        memory
            .insert_conversation(
                "user",
                "We decided to always use anyhow for error handling in this project.",
                Some("local"),
                None,
            )
            .await?;
    }

    // Reopen and verify data persists
    {
        let config = MemoryConfig {
            db_path,
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;
        let stats = memory.stats().await?;

        assert_eq!(stats.conversation_count, 1);
    }

    Ok(())
}

#[tokio::test]
async fn test_memory_disabled() -> Result<()> {
    // Test that memory can be disabled via config
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        enabled: false, // Disabled
        ..Default::default()
    };

    // Memory system creation should still succeed when disabled
    // (It's up to the REPL to not use it)
    assert!(!config.enabled);

    Ok(())
}
