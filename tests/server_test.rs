// Integration tests for HTTP server

#[tokio::test]
async fn test_session_manager() {
    use finch::server::SessionManager;

    let manager = SessionManager::new(10, 30);

    // Create a session
    let session1 = manager.get_or_create(None).unwrap();
    assert_eq!(manager.active_count(), 1);

    // Retrieve the same session
    let session2 = manager.get_or_create(Some(&session1.id)).unwrap();
    assert_eq!(session1.id, session2.id);
    assert_eq!(manager.active_count(), 1); // Still only 1 session

    // Create a new session
    let session3 = manager.get_or_create(None).unwrap();
    assert_ne!(session1.id, session3.id);
    assert_eq!(manager.active_count(), 2);

    // Delete a session
    assert!(manager.delete(&session1.id));
    assert_eq!(manager.active_count(), 1);
}
