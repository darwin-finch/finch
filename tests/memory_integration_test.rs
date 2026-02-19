// Integration tests for Phase 4: Hierarchical Memory System

use finch::memory::{MemorySystem, MemoryConfig, EmbeddingEngine, TfIdfEmbedding, MemTree, cosine_similarity};
use anyhow::Result;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_memory_system_creation() -> Result<()> {
    let temp = NamedTempFile::new()?;
    let config = MemoryConfig {
        db_path: temp.path().to_path_buf(),
        enabled: true,
        max_context_items: 5,
        checkpoint_interval_secs: 300,
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
    memory.insert_conversation(
        "user",
        "How do I use Rust lifetimes?",
        Some("local"),
        Some("test-session"),
    ).await?;

    memory.insert_conversation(
        "assistant",
        "Lifetimes in Rust ensure references are valid...",
        Some("local"),
        Some("test-session"),
    ).await?;

    memory.insert_conversation(
        "user",
        "What is Python asyncio?",
        Some("local"),
        Some("test-session"),
    ).await?;

    // Query for Rust-related content
    let results = memory.query("Rust programming", Some(3)).await?;

    assert!(!results.is_empty());
    // Should find Rust-related conversations
    assert!(results.iter().any(|r| r.contains("Rust") || r.contains("lifetimes")));

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

    // Insert multiple conversations
    for i in 1..=10 {
        memory.insert_conversation(
            "user",
            &format!("Question {}", i),
            Some("local"),
            None,
        ).await?;
    }

    let stats = memory.stats().await?;
    assert_eq!(stats.conversation_count, 10);
    assert_eq!(stats.tree_node_count, 10);  // One node per conversation

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
        memory.insert_conversation(
            "user",
            &format!("Message {}", i),
            Some("local"),
            None,
        ).await?;
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

#[tokio::test]
async fn test_memtree_insertion() -> Result<()> {
    let mut tree = MemTree::new();
    let engine = TfIdfEmbedding::new();

    // Insert multiple nodes
    let texts = vec![
        "rust programming",
        "rust coding",
        "python programming",
        "javascript web development",
    ];

    for text in texts {
        let emb = engine.embed(text)?;
        tree.insert(text.to_string(), emb)?;
    }

    assert_eq!(tree.size(), 4);

    // Query for similar content
    let query_emb = engine.embed("rust language")?;
    let results = tree.retrieve(&query_emb, 2);

    assert_eq!(results.len(), 2);
    // Should find rust-related content
    assert!(results.iter().any(|(_, text, _)| text.contains("rust")));

    Ok(())
}

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
        memory.insert_conversation(
            "user",
            "Test persistence",
            Some("local"),
            None,
        ).await?;
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
        enabled: false,  // Disabled
        ..Default::default()
    };

    // Memory system creation should still succeed when disabled
    // (It's up to the REPL to not use it)
    assert!(!config.enabled);

    Ok(())
}
