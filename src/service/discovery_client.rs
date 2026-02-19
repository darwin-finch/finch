// Service discovery client
//
// Discovers Finch daemons on local network

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::time::{Duration, Instant};

use super::discovery::SERVICE_TYPE;

/// A discovered Finch service
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: String,
    pub description: String,
    pub capabilities: Vec<String>,
}

/// Client for discovering Finch services
pub struct ServiceDiscoveryClient {
    daemon: ServiceDaemon,
}

impl ServiceDiscoveryClient {
    /// Create new service discovery client
    pub fn new() -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .context("Failed to create mDNS service daemon")?;

        Ok(Self { daemon })
    }

    /// Discover services on local network (blocks for timeout duration)
    pub fn discover(&self, timeout: Duration) -> Result<Vec<DiscoveredService>> {
        tracing::info!("Discovering Finch instances on local network...");

        // Start browsing for services
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .context("Failed to start mDNS browsing")?;

        let deadline = Instant::now() + timeout;
        let mut services = Vec::new();

        // Poll for service events until timeout
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());

            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    // Extract service properties
                    let model = info
                        .get_property_val_str("model")
                        .unwrap_or("unknown")
                        .to_string();

                    let description = info
                        .get_property_val_str("description")
                        .unwrap_or("")
                        .to_string();

                    let capabilities_str = info
                        .get_property_val_str("capabilities")
                        .unwrap_or("")
                        .to_string();

                    let capabilities: Vec<String> = if capabilities_str.is_empty() {
                        Vec::new()
                    } else {
                        capabilities_str.split(',').map(|s| s.to_string()).collect()
                    };

                    // Get first IP address (prefer IPv4)
                    let host = info
                        .get_addresses()
                        .iter()
                        .find(|addr| addr.is_ipv4())
                        .or_else(|| info.get_addresses().iter().next())
                        .map(|addr| addr.to_string())
                        .unwrap_or_else(|| info.get_hostname().to_string());

                    let service = DiscoveredService {
                        name: info.get_fullname().to_string(),
                        host,
                        port: info.get_port(),
                        model,
                        description,
                        capabilities,
                    };

                    tracing::debug!("Discovered service: {:?}", service);
                    services.push(service);
                }
                Ok(ServiceEvent::ServiceRemoved(_, _)) => {
                    // Service went away - we don't track removals for now
                    continue;
                }
                Ok(_) => {
                    // Other events (SearchStarted, etc.) - continue polling
                    continue;
                }
                Err(e) => {
                    // Timeout or channel disconnected
                    tracing::debug!("Service discovery polling ended: {}", e);
                    break;
                }
            }
        }

        tracing::info!("Discovered {} Finch instance(s)", services.len());
        Ok(services)
    }
}

impl Default for ServiceDiscoveryClient {
    fn default() -> Self {
        Self::new().expect("Failed to create service discovery client")
    }
}
