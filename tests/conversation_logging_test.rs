// Integration tests for Phase 1: Conversation Logging

use anyhow::Result;
use finch::logging::ConversationLogger;
use std::fs;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_conversation_logger_creation() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    // Create logger
    let logger = ConversationLogger::new(log_path.clone())?;

    // Verify file exists
    assert!(log_path.exists(), "Log file should be created");

    Ok(())
}

#[tokio::test]
async fn test_log_single_interaction() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    let mut logger = ConversationLogger::new(log_path.clone())?;

    // Log an interaction
    logger
        .log_interaction("What is 2+2?", "4", "local", &vec!["Read".to_string()])
        .await?;

    // Force flush
    drop(logger);

    // Read log file
    let contents = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = contents.lines().collect();

    // Should have 1 log entry
    assert_eq!(lines.len(), 1, "Should have exactly one log entry");

    // Parse JSON
    let entry: serde_json::Value = serde_json::from_str(lines[0])?;

    // Verify fields
    assert_eq!(entry["query"].as_str().unwrap(), "What is 2+2?");
    assert_eq!(entry["response"].as_str().unwrap(), "4");
    assert_eq!(entry["model"].as_str().unwrap(), "local");
    assert!(entry["timestamp"].is_string());
    assert!(entry["id"].is_string());

    // Tools used
    let tools = entry["tools_used"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].as_str().unwrap(), "Read");

    Ok(())
}

#[tokio::test]
async fn test_log_multiple_interactions() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    let mut logger = ConversationLogger::new(log_path.clone())?;

    // Log multiple interactions
    logger
        .log_interaction("Query 1", "Response 1", "local", &vec![])
        .await?;
    logger
        .log_interaction(
            "Query 2",
            "Response 2",
            "teacher",
            &vec!["Bash".to_string()],
        )
        .await?;
    logger
        .log_interaction("Query 3", "Response 3", "local", &vec![])
        .await?;

    // Force flush
    drop(logger);

    // Read log file
    let contents = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = contents.lines().collect();

    // Should have 3 log entries
    assert_eq!(lines.len(), 3, "Should have exactly three log entries");

    // Verify each entry is valid JSON
    for line in lines {
        let entry: serde_json::Value = serde_json::from_str(line)?;
        assert!(entry["query"].is_string());
        assert!(entry["response"].is_string());
        assert!(entry["model"].is_string());
    }

    Ok(())
}

#[tokio::test]
async fn test_log_with_feedback() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    let mut logger = ConversationLogger::new(log_path.clone())?;

    // Log interaction
    logger
        .log_interaction("Test query", "Test response", "local", &vec![])
        .await?;

    // Get last log ID (for adding feedback)
    // Note: This would normally be done by the REPL /feedback commands

    drop(logger);

    // Verify weight field exists and has default value
    let contents = fs::read_to_string(&log_path)?;
    let entry: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap())?;

    assert!(entry.get("weight").is_some(), "Weight field should exist");
    assert_eq!(
        entry["weight"].as_f64().unwrap(),
        1.0,
        "Default weight should be 1.0"
    );

    // Feedback field is omitted if None (due to skip_serializing_if)
    // This is correct behavior - feedback is only serialized when present

    Ok(())
}

#[tokio::test]
async fn test_buffer_flushing() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    let mut logger = ConversationLogger::new(log_path.clone())?;

    // Log one interaction
    logger
        .log_interaction("Query", "Response", "local", &vec![])
        .await?;

    // File should be empty initially (buffered)
    // After enough writes or explicit flush, it should be written

    // Force flush by dropping
    drop(logger);

    // Now file should have content
    let contents = fs::read_to_string(&log_path)?;
    assert!(!contents.is_empty(), "File should have content after flush");

    Ok(())
}

#[tokio::test]
async fn test_jsonl_format() -> Result<()> {
    let temp_file = NamedTempFile::new()?;
    let log_path = temp_file.path().to_path_buf();

    let mut logger = ConversationLogger::new(log_path.clone())?;

    // Log multiple interactions
    for i in 1..=5 {
        logger
            .log_interaction(
                &format!("Query {}", i),
                &format!("Response {}", i),
                if i % 2 == 0 { "teacher" } else { "local" },
                &vec![],
            )
            .await?;
    }

    drop(logger);

    // Read and verify JSONL format
    let contents = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = contents.lines().collect();

    assert_eq!(lines.len(), 5, "Should have 5 lines");

    // Each line should be valid JSON
    for (i, line) in lines.iter().enumerate() {
        let entry: serde_json::Value =
            serde_json::from_str(line).expect(&format!("Line {} should be valid JSON", i + 1));

        // Verify structure
        assert!(entry.is_object());
        assert!(entry["id"].is_string());
        assert!(entry["timestamp"].is_string());
        assert!(entry["query"].is_string());
        assert!(entry["response"].is_string());
        assert!(entry["model"].is_string());
        assert!(entry["tools_used"].is_array());
    }

    Ok(())
}
