// Integration tests for HTTP server

use finch::{
    claude::ClaudeClient,
    config::Config,
    metrics::MetricsLogger,
    models::ThresholdRouter,
    router::Router,
    server::{AgentServer, ServerConfig},
};
use std::sync::Arc;

#[tokio::test]
async fn test_server_creation() {
    // Create test configuration
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| "test_key".to_string());

    let config = Config::new(api_key.clone());

    // Create components
    let threshold_router = ThresholdRouter::new();
    let router = Router::new(threshold_router);
    let claude_client = ClaudeClient::new(api_key).expect("Failed to create Claude client");
    let metrics_logger = MetricsLogger::new(std::env::temp_dir().join("shammah_test_metrics"))
        .expect("Failed to create metrics logger");

    // Create server config
    let server_config = ServerConfig {
        bind_address: "127.0.0.1:0".to_string(), // Use port 0 for test
        max_sessions: 10,
        session_timeout_minutes: 30,
        auth_enabled: false,
        api_keys: vec![],
    };

    // Create server
    let server = AgentServer::new(config, server_config, claude_client, router, metrics_logger);

    assert!(server.is_ok(), "Server should be created successfully");
}

#[test]
fn test_session_manager() {
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
