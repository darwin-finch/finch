// mDNS/Bonjour service advertisement
//
// Advertises Finch daemon on local network for auto-discovery

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

/// Service type for mDNS (Bonjour)
pub const SERVICE_TYPE: &str = "_finch._tcp.local.";

/// Service configuration for advertisement
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub name: String,
    pub description: String,
    pub model: String,
    pub capabilities: Vec<String>,
}

/// Service discovery via mDNS (Bonjour)
pub struct ServiceDiscovery {
    daemon: ServiceDaemon,
    config: ServiceConfig,
    instance_name: String,
}

impl ServiceDiscovery {
    /// Create new service discovery
    pub fn new(config: ServiceConfig) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("Failed to create mDNS service daemon")?;

        // Generate instance name from hostname
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "finch".to_string());

        let instance_name = if config.name.is_empty() {
            format!("finch-{}", hostname)
        } else {
            config.name.clone()
        };

        tracing::debug!(
            "Created mDNS service daemon with instance: {}",
            instance_name
        );

        Ok(Self {
            daemon,
            config,
            instance_name,
        })
    }

    /// Advertise service on local network
    pub fn advertise(&self, port: u16) -> Result<()> {
        // Get hostname for service registration
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "localhost".to_string());

        // Build TXT record properties
        let mut properties = HashMap::new();
        properties.insert("model".to_string(), self.config.model.clone());
        properties.insert("description".to_string(), self.config.description.clone());
        properties.insert(
            "capabilities".to_string(),
            self.config.capabilities.join(","),
        );
        properties.insert("version".to_string(), env!("CARGO_PKG_VERSION").to_string());

        // Create service info
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &self.instance_name,
            &format!("{}.", hostname),
            (), // Use default IP
            port,
            Some(properties),
        )
        .context("Failed to create service info")?;

        // Register service
        self.daemon
            .register(service_info)
            .context("Failed to register mDNS service")?;

        tracing::info!(
            "Advertising service: {} on port {} (model: {}, capabilities: {})",
            self.instance_name,
            port,
            self.config.model,
            self.config.capabilities.join(", ")
        );

        Ok(())
    }

    /// Stop advertising
    pub fn stop(&self) -> Result<()> {
        // Shutdown unregisters all services
        self.daemon
            .shutdown()
            .context("Failed to stop mDNS service")?;

        tracing::info!("Stopped advertising service: {}", self.instance_name);
        Ok(())
    }
}
