// Node Identity — persistent UUID for each finch instance.
//
// Every finch node gets a stable UUID written to ~/.finch/node_id on first
// run. This identity is used for:
//   - mDNS/network advertisement
//   - Work attribution in distributed mode
//   - Future: points and reputation on the worker network

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// A finch node's stable identity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// Stable UUID — never changes after first run
    pub id: Uuid,
    /// Human-readable name (defaults to hostname, user-configurable)
    pub name: String,
    /// Finch version this node is running
    pub version: String,
}

impl NodeIdentity {
    /// Load existing identity or create one on first run.
    /// Persists to `~/.finch/node_id`.
    pub fn load_or_create() -> Result<Self> {
        let path = Self::path()?;

        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read node identity from {}", path.display()))?;
            let id: Self = serde_json::from_str(&raw)
                .with_context(|| "Failed to parse node identity JSON")?;
            return Ok(id);
        }

        // First run — generate a new identity
        let identity = Self::generate()?;
        identity.save()?;
        tracing::info!(node_id = %identity.id, "Generated new node identity");
        Ok(identity)
    }

    fn generate() -> Result<Self> {
        let id = Uuid::new_v4();
        let name = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| format!("finch-{}", &id.to_string()[..8]));
        Ok(Self {
            id,
            name,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create ~/.finch directory")?;
        }
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize node identity")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write node identity to {}", path.display()))?;
        Ok(())
    }

    fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        Ok(home.join(".finch").join("node_id"))
    }

    /// Short display prefix (first 8 chars of UUID)
    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_identity_roundtrip() {
        let original = NodeIdentity {
            id: Uuid::new_v4(),
            name: "test-node".to_string(),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(original.id, parsed.id);
        assert_eq!(original.name, parsed.name);
    }

    #[test]
    fn test_short_id() {
        let id = NodeIdentity {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
        };
        assert_eq!(id.short_id(), "550e8400");
        assert_eq!(id.short_id().len(), 8);
    }
}
