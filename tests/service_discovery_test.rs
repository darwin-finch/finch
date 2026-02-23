// Integration tests for Phase 3: Service Discovery (mDNS/Bonjour)

use anyhow::Result;
use finch::service::{ServiceConfig, ServiceDiscovery, ServiceDiscoveryClient};
use std::time::Duration;

#[test]
fn test_service_discovery_creation() -> Result<()> {
    // Test creating a service discovery instance
    let config = ServiceConfig {
        name: "test-finch".to_string(),
        description: "Test Finch instance".to_string(),
        model: "Qwen-2.5-7B".to_string(),
        capabilities: vec!["code".to_string(), "general".to_string()],
    };

    let discovery = ServiceDiscovery::new(config)?;

    // Just verify it was created successfully
    // (We won't actually advertise in tests to avoid network effects)
    Ok(())
}

#[test]
fn test_service_config_builder() -> Result<()> {
    // Test building a service configuration
    let config = ServiceConfig {
        name: "finch-test-machine".to_string(),
        description: "Finch on test machine".to_string(),
        model: "Qwen-2.5-1.5B".to_string(),
        capabilities: vec!["code".to_string()],
    };

    assert_eq!(config.name, "finch-test-machine");
    assert_eq!(config.model, "Qwen-2.5-1.5B");
    assert_eq!(config.capabilities.len(), 1);

    Ok(())
}

#[test]
fn test_discovery_client_creation() -> Result<()> {
    // Test creating a discovery client
    let client = ServiceDiscoveryClient::new()?;

    // Client creation should succeed
    Ok(())
}

#[test]
fn test_discovery_timeout() -> Result<()> {
    // Test that discovery respects timeout
    // (Won't find any services in test environment, should complete quickly)
    let client = ServiceDiscoveryClient::new()?;

    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(100);

    let services = client.discover(timeout)?;

    let elapsed = start.elapsed();

    // Should complete within timeout + small margin
    assert!(elapsed < timeout + Duration::from_millis(500));

    // No services expected in test environment
    assert_eq!(services.len(), 0);

    Ok(())
}

#[test]
fn test_service_config_empty_name() -> Result<()> {
    // Test that empty service name is allowed (will be auto-generated)
    let config = ServiceConfig {
        name: String::new(),
        description: "Test".to_string(),
        model: "Qwen-2.5-3B".to_string(),
        capabilities: vec![],
    };

    // Should create discovery even with empty name (will use hostname)
    let discovery = ServiceDiscovery::new(config)?;

    Ok(())
}

#[test]
fn test_service_capabilities_list() -> Result<()> {
    // Test various capability configurations
    let configs = vec![
        vec!["code", "general", "tool-use"],
        vec!["code"],
        vec![], // Empty capabilities
    ];

    for caps in configs {
        let config = ServiceConfig {
            name: "test".to_string(),
            description: "Test".to_string(),
            model: "Qwen".to_string(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
        };

        // Should create successfully with any capability list
        let _discovery = ServiceDiscovery::new(config)?;
    }

    Ok(())
}

// Note: We don't test actual mDNS advertisement/discovery here because:
// 1. It requires network access (may not work in CI)
// 2. It would interfere with real Finch instances on the network
// 3. Integration testing is better done manually or with docker compose
//
// Manual testing instructions:
// 1. Terminal 1: Start daemon with advertise=true in config
//    cargo run -- daemon
// 2. Terminal 2: Run discovery command
//    cargo run -- query "/discover"
// 3. Verify the daemon shows up in discovery results
