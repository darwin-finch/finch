// Regression coverage for the disabled legacy conversation collector.

use anyhow::Result;
use finch::logging::conversation_logger::AutomaticConversationLoggingDisabled;
use finch::logging::{ConversationLogger, Feedback};

#[test]
fn test_conversation_logger_creation_is_side_effect_free() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("legacy/conversations.jsonl");

    let logger = ConversationLogger::new(log_path.clone())?;

    assert_eq!(logger.path(), &log_path);
    assert!(!log_path.exists());
    assert!(!log_path.parent().unwrap().exists());
    Ok(())
}

#[tokio::test]
async fn test_legacy_conversation_persistence_fails_closed_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("conversations.jsonl");
    std::fs::write(&log_path, "preserved legacy data\n")?;
    let before = std::fs::read(&log_path)?;
    let mut logger = ConversationLogger::new(log_path.clone())?;

    let error = logger
        .log_interaction("ordinary query", "ordinary response", "teacher", &[])
        .await
        .unwrap_err();

    assert!(error
        .downcast_ref::<AutomaticConversationLoggingDisabled>()
        .is_some());
    assert!(logger.flush().await.is_err());
    assert!(logger
        .add_feedback("legacy-id", Feedback::Critical)
        .await
        .is_err());
    drop(logger);
    assert_eq!(std::fs::read(&log_path)?, before);
    Ok(())
}
