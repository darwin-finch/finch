// mDNS/UPnP service advertisement
//
// Advertises Shammah daemon on local network for auto-discovery

use anyhow::Result;

/// Service configuration for advertisement
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub name: String,
    pub description: String,
    pub model: String,
    pub capabilities: Vec<String>,
}

/// Service discovery via mDNS
pub struct ServiceDiscovery {
    config: ServiceConfig,
}

impl ServiceDiscovery {
    /// Create new service discovery
    pub fn new(config: ServiceConfig) -> Result<Self> {
        Ok(Self { config })
    }

    /// Advertise service on local network
    pub fn advertise(&self, _port: u16) -> Result<()> {
        // TODO: Use mdns-sd crate to advertise _shammah._tcp.local.
        // TODO: Include properties: model, description, capabilities
        tracing::info!(
            "Advertising service: {} ({})",
            self.config.name,
            self.config.model
        );
        Ok(())
    }

    /// Stop advertising
    pub fn stop(&self) -> Result<()> {
        // TODO: Unregister service
        Ok(())
    }
}
