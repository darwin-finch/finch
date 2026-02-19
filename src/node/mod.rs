// Node module — identity, capabilities, and work statistics.
//
// Every finch instance is a node. Nodes have:
//   - A stable UUID (persisted to ~/.finch/node_id)
//   - Capabilities (what models it can run, what RAM it has)
//   - Work statistics (queries processed, latency, local vs. teacher)
//
// This is the foundation for the distributed worker network where old
// laptops accept delegated work and earn reputation.

pub mod identity;
pub mod stats;

pub use identity::NodeIdentity;
pub use stats::{WorkStats, WorkTracker};

use crate::models::model_selector::{ModelSelection, ModelSelector};
use serde::{Deserialize, Serialize};

/// Full description of this node's capabilities — advertised via mDNS
/// and returned by the /v1/node/info endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub identity: NodeIdentity,
    pub capabilities: NodeCapabilities,
}

/// What this node can do
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// RAM in GB
    pub ram_gb: usize,
    /// Local model available (None = cloud-only mode)
    pub local_model: Option<String>,
    /// Whether a teacher API is configured
    pub has_teacher_api: bool,
    /// Finch version
    pub version: String,
    /// Operating system
    pub os: String,
}

impl NodeCapabilities {
    pub fn detect(has_teacher_api: bool) -> Self {
        let ram_gb = ModelSelector::get_total_ram_gb();
        let local_model = match ModelSelector::select_for_system() {
            Ok(ModelSelection::Local(size)) => Some(size.description().to_string()),
            _ => None,
        };

        Self {
            ram_gb,
            local_model,
            has_teacher_api,
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
        }
    }

    pub fn is_cloud_only(&self) -> bool {
        self.local_model.is_none()
    }
}

impl NodeInfo {
    pub fn load(has_teacher_api: bool) -> anyhow::Result<Self> {
        Ok(Self {
            identity: NodeIdentity::load_or_create()?,
            capabilities: NodeCapabilities::detect(has_teacher_api),
        })
    }

    /// One-line summary for status display
    pub fn summary(&self) -> String {
        let model = self.capabilities.local_model
            .as_deref()
            .unwrap_or("cloud-only");
        format!(
            "node:{} | {} | {}GB RAM | {}",
            self.identity.short_id(),
            model,
            self.capabilities.ram_gb,
            self.capabilities.os,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_capabilities_detect() {
        let caps = NodeCapabilities::detect(false);
        assert!(caps.ram_gb >= 1);
        assert!(!caps.version.is_empty());
        assert!(!caps.os.is_empty());
    }

    #[test]
    fn test_node_capabilities_cloud_only_when_no_model() {
        let caps = NodeCapabilities {
            ram_gb: 1,
            local_model: None,
            has_teacher_api: true,
            version: "test".to_string(),
            os: "test".to_string(),
        };
        assert!(caps.is_cloud_only());
    }
}
