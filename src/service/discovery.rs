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
    /// Public Ed25519 transport identity. Safe to advertise; it grants no
    /// authority and is compared against authenticated invitation/channel data.
    pub node_public_key: [u8; 32],
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
        let _hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "finch".to_string());

        // Use the stable cute name (e.g. "tiny-bird") when no explicit name is set.
        // This makes peer discovery show "tiny-bird is here" instead of "macbook-pro is here".
        let instance_name = if config.name.is_empty() {
            format!("finch-{}", crate::node_name::NAME.as_str())
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

        let properties = advertised_properties(&self.config);

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
            "Advertising Finch node discovery endpoint: {} on port {}",
            self.instance_name,
            port
        );

        Ok(())
    }

    /// Stop advertising
    pub fn stop(&self) -> Result<()> {
        // Shutdown unregisters all services.
        // Receive the shutdown-complete response so the daemon thread can finish
        // cleanly — without this recv the thread logs "sending on a closed channel".
        let shutdown_rx = self
            .daemon
            .shutdown()
            .context("Failed to stop mDNS service")?;
        let _ = shutdown_rx.recv_timeout(std::time::Duration::from_millis(200));

        tracing::info!("Stopped advertising service: {}", self.instance_name);
        Ok(())
    }
}

/// Public discovery metadata is deliberately an authority-free allowlist.
///
/// mDNS TXT records are visible to every listener on the local network. They
/// may describe how to reach Finch, but must never contain credentials that
/// authorize a listener to use it.
fn advertised_properties(config: &ServiceConfig) -> HashMap<String, String> {
    HashMap::from([
        ("description".to_string(), config.description.clone()),
        ("version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        // Cute node name — shown in the TUI when this machine is discovered.
        ("name".to_string(), crate::node_name::NAME.clone()),
        ("node_key".to_string(), hex::encode(config.node_public_key)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdns_properties_are_an_authority_free_allowlist() {
        let config = ServiceConfig {
            name: "test-finch".into(),
            description: "test service".into(),
            node_public_key: [7; 32],
        };

        let properties = advertised_properties(&config);
        let mut keys = properties.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();

        assert_eq!(keys, ["description", "name", "node_key", "version"]);
        assert_eq!(properties["node_key"], hex::encode([7; 32]));
        for forbidden in ["token", "password", "credential", "secret", "api_key"] {
            assert!(!properties.contains_key(forbidden));
        }
    }
}
