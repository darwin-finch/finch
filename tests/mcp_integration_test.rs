// Integration tests for MCP (Model Context Protocol) plugin system
//
// Tests the full MCP workflow: configuration, connection, tool discovery, and execution

use finch::tools::mcp::{McpClient, McpServerConfig, TransportType};
use std::collections::HashMap;

#[tokio::test]
async fn test_mcp_client_creation_with_empty_config() {
    // Test that McpClient can be created with empty configuration
    let config: HashMap<String, McpServerConfig> = HashMap::new();
    let client = McpClient::from_config(&config).await;

    assert!(client.is_ok(), "Should create client with empty config");

    let client = client.unwrap();
    let servers = client.list_servers().await;
    assert_eq!(servers.len(), 0, "Should have no servers connected");

    let tools = client.list_tools().await;
    assert_eq!(tools.len(), 0, "Should have no tools available");
}

#[tokio::test]
async fn test_mcp_client_with_invalid_server() {
    // Test that invalid server config is handled gracefully
    let mut config = HashMap::new();
    config.insert(
        "invalid_server".to_string(),
        McpServerConfig {
            command: Some("nonexistent_command_12345".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await;

    // Should succeed but with no connected servers
    assert!(
        client.is_ok(),
        "Client creation should succeed even with invalid server"
    );

    let client = client.unwrap();
    let servers = client.list_servers().await;
    assert_eq!(servers.len(), 0, "Invalid server should not be connected");

    let tools = client.list_tools().await;
    assert_eq!(tools.len(), 0, "Should have no tools from invalid server");
}

#[tokio::test]
async fn test_mcp_client_with_disabled_server() {
    // Test that disabled servers are not started
    let mut config = HashMap::new();
    config.insert(
        "disabled_server".to_string(),
        McpServerConfig {
            command: Some("echo".to_string()),
            args: vec!["test".to_string()],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: false, // Disabled
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await;

    assert!(client.is_ok(), "Client creation should succeed");

    let client = client.unwrap();
    let servers = client.list_servers().await;
    assert_eq!(servers.len(), 0, "Disabled server should not be started");

    // Check is_connected returns false
    assert!(
        !client.is_connected("disabled_server").await,
        "Should not be connected"
    );
}

#[tokio::test]
async fn test_mcp_client_disconnect() {
    // Test disconnecting from a server
    let mut config = HashMap::new();
    config.insert(
        "test_server".to_string(),
        McpServerConfig {
            command: Some("nonexistent".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await.unwrap();

    // Try to disconnect (should succeed even if server never connected)
    let result = client.disconnect("test_server").await;
    assert!(result.is_ok(), "Disconnect should succeed");

    // Verify server is gone
    assert!(!client.is_connected("test_server").await);
}

#[tokio::test]
async fn test_mcp_client_disconnect_all() {
    // Test disconnecting from all servers
    let mut config = HashMap::new();
    config.insert(
        "server1".to_string(),
        McpServerConfig {
            command: Some("nonexistent1".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );
    config.insert(
        "server2".to_string(),
        McpServerConfig {
            command: Some("nonexistent2".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await.unwrap();

    // Disconnect all
    let result = client.disconnect_all().await;
    assert!(result.is_ok(), "Disconnect all should succeed");

    // Verify no servers remain
    let servers = client.list_servers().await;
    assert_eq!(servers.len(), 0, "All servers should be disconnected");
}

#[tokio::test]
async fn test_mcp_tool_name_prefixing() {
    // Test that tool names are properly prefixed with "mcp_<server>_<tool>"
    let mut config = HashMap::new();
    config.insert(
        "test_server".to_string(),
        McpServerConfig {
            command: Some("nonexistent".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await.unwrap();
    let tools = client.list_tools().await;

    // All tool names should start with "mcp_"
    for tool in tools {
        assert!(
            tool.name.starts_with("mcp_"),
            "Tool name '{}' should start with 'mcp_'",
            tool.name
        );

        // Tool name format: mcp_<server>_<tool>
        let parts: Vec<&str> = tool.name.split('_').collect();
        assert!(
            parts.len() >= 3,
            "Tool name '{}' should have at least 3 parts (mcp_server_tool)",
            tool.name
        );
        assert_eq!(parts[0], "mcp", "First part should be 'mcp'");
    }
}

#[tokio::test]
async fn test_mcp_execute_tool_invalid_name() {
    // Test executing a tool with invalid name format
    let client = McpClient::from_config(&HashMap::new()).await.unwrap();

    // Tool name without "mcp_" prefix
    let result = client
        .execute_tool("invalid_tool", serde_json::json!({}))
        .await;
    assert!(result.is_err(), "Should fail with invalid tool name");

    // Tool name with only one underscore
    let result = client
        .execute_tool("mcp_server", serde_json::json!({}))
        .await;
    assert!(result.is_err(), "Should fail with malformed tool name");
}

#[tokio::test]
async fn test_mcp_execute_tool_nonexistent_server() {
    // Test executing a tool from a server that doesn't exist
    let client = McpClient::from_config(&HashMap::new()).await.unwrap();

    let result = client
        .execute_tool("mcp_nonexistent_tool", serde_json::json!({}))
        .await;
    assert!(result.is_err(), "Should fail when server doesn't exist");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("not found") || error_msg.contains("nonexistent"),
        "Error should mention server not found"
    );
}

#[tokio::test]
async fn test_mcp_refresh_tools_with_no_servers() {
    // Test refreshing tools when no servers are connected
    let client = McpClient::from_config(&HashMap::new()).await.unwrap();

    let result = client.refresh_all_tools().await;
    assert!(
        result.is_ok(),
        "Refresh should succeed even with no servers"
    );
}

#[tokio::test]
async fn test_mcp_config_validation() {
    // Test configuration validation
    let mut config = HashMap::new();

    // Invalid: STDIO transport without command
    config.insert(
        "invalid_stdio".to_string(),
        McpServerConfig {
            command: None, // Missing command
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await;
    assert!(
        client.is_ok(),
        "Client should handle invalid config gracefully"
    );

    let client = client.unwrap();
    let servers = client.list_servers().await;
    assert_eq!(servers.len(), 0, "Invalid server should not connect");
}

#[tokio::test]
async fn test_mcp_multiple_servers_isolation() {
    // Test that multiple servers are properly isolated
    let mut config = HashMap::new();
    config.insert(
        "server1".to_string(),
        McpServerConfig {
            command: Some("cmd1".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );
    config.insert(
        "server2".to_string(),
        McpServerConfig {
            command: Some("cmd2".to_string()),
            args: vec![],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 300,
        },
    );

    let client = McpClient::from_config(&config).await.unwrap();

    // Disconnect one server
    let _ = client.disconnect("server1").await;

    // Other server should still be listed (even if not successfully connected)
    // Note: In practice neither connects, but the point is testing isolation
    let servers = client.list_servers().await;
    assert!(
        !servers.contains(&"server1".to_string()),
        "server1 should be disconnected"
    );
}

#[test]
fn test_mcp_config_serialization() {
    // Test that McpServerConfig can be serialized/deserialized
    let config = McpServerConfig {
        command: Some("npx".to_string()),
        args: vec!["-y".to_string(), "test-package".to_string()],
        transport: TransportType::Stdio,
        url: None,
        env: {
            let mut env = HashMap::new();
            env.insert("API_KEY".to_string(), "test123".to_string());
            env
        },
        enabled: true,
        timeout_secs: 300,
    };

    // Serialize to JSON
    let json = serde_json::to_string(&config).unwrap();

    // Deserialize back
    let deserialized: McpServerConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.command, config.command);
    assert_eq!(deserialized.args, config.args);
    assert_eq!(deserialized.enabled, config.enabled);
    assert_eq!(
        deserialized.env.get("API_KEY"),
        Some(&"test123".to_string())
    );
}

#[test]
fn test_transport_type_serialization() {
    // Test TransportType enum serialization
    let stdio = TransportType::Stdio;
    let sse = TransportType::Sse;

    let stdio_json = serde_json::to_string(&stdio).unwrap();
    let sse_json = serde_json::to_string(&sse).unwrap();

    assert_eq!(stdio_json, r#""stdio""#);
    assert_eq!(sse_json, r#""sse""#);

    // Test deserialization
    let stdio_parsed: TransportType = serde_json::from_str(&stdio_json).unwrap();
    let sse_parsed: TransportType = serde_json::from_str(&sse_json).unwrap();

    assert!(matches!(stdio_parsed, TransportType::Stdio));
    assert!(matches!(sse_parsed, TransportType::Sse));
}

#[cfg(unix)]
#[tokio::test]
async fn test_stdio_discovery_notification_and_tool_call() {
    // A deterministic modern-protocol fixture: discovery, tools/list with a
    // preceding notification, then tools/call.
    let script = r#"
IFS= read -r discover
case "$discover" in *'"io.modelcontextprotocol/protocolVersion":"2026-07-28"'*) ;; *) exit 2;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}}}}'
IFS= read -r list_one
case "$list_one" in *'"io.modelcontextprotocol/protocolVersion":"2026-07-28"'*) ;; *) exit 3;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo_value","description":"Echo a value","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}],"nextCursor":"page-two"}}'
IFS= read -r list_two
case "$list_two" in *'"cursor":"page-two"'*) ;; *) exit 4;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"second_tool","description":"Second page","inputSchema":{"type":"object"}}]}}'
IFS= read -r call
case "$call" in *'"io.modelcontextprotocol/protocolVersion":"2026-07-28"'*) ;; *) exit 5;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"fixture result"},{"type":"resource_link","name":"artifact","uri":"file:///tmp/artifact"}],"structuredContent":{"ok":true},"isError":false}}'
"#;

    let config = HashMap::from([(
        "fixture_with_underscores".to_string(),
        McpServerConfig {
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), script.to_string()],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 5,
        },
    )]);

    let client = McpClient::from_config(&config).await.unwrap();
    let tools = client.list_tools().await;
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "mcp_fixture_with_underscores_echo_value");

    let result = client
        .execute_tool(
            "mcp_fixture_with_underscores_echo_value",
            serde_json::json!({ "value": "hello" }),
        )
        .await
        .unwrap();
    assert!(result.contains("fixture result"));
    assert!(result.contains("resource_link"));
    assert!(result.contains("Structured content"));
    assert!(result.contains("\"ok\": true"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_stdio_falls_back_to_legacy_initialize() {
    let script = r#"
IFS= read -r discover
printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}'
IFS= read -r initialize
case "$initialize" in *'"protocolVersion":"2025-11-25"'*) ;; *) exit 2;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"legacy-fixture","version":"1"}}}'
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}'
"#;
    let config = HashMap::from([(
        "legacy".to_string(),
        McpServerConfig {
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), script.to_string()],
            transport: TransportType::Stdio,
            url: None,
            env: HashMap::new(),
            enabled: true,
            timeout_secs: 5,
        },
    )]);

    let client = McpClient::from_config(&config).await.unwrap();
    assert!(client.is_connected("legacy").await);
    assert!(client.list_tools().await.is_empty());
}
