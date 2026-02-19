// Service discovery and daemon-only mode
//
// Enables distributed AI assistant across multiple machines

pub mod discovery;
pub mod discovery_client;

pub use discovery::{ServiceDiscovery, ServiceConfig};
pub use discovery_client::{DiscoveredService, ServiceDiscoveryClient};
