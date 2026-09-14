# node — public interface

Generated from [`src/node/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/node/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub struct IsolatedNodeRootSwap { … }
impl IsolatedNodeRootSwap {
    pub fn external_sentinel_unchanged(&self) -> bool;
    pub fn pinned_node_id_exists(&self) -> bool;
    pub fn replacement_node_id_exists(&self) -> bool;
}
/// Opaque disposable state owned by worker-node integration tests.
pub struct IsolatedNodeTestState { … }
impl IsolatedNodeTestState {
    pub fn fifo_node_id_fixture(&self) -> anyhow::Result<()>;
    pub fn hardlink_node_id_fixture(&self, source: &std::path::Path) -> anyhow::Result<()>;
    pub fn load_node_info(&self, capabilities: NodeCapabilities) -> anyhow::Result<NodeInfo>;
    pub fn new() -> anyhow::Result<Self>;
    pub fn node_id_exists(&self) -> anyhow::Result<bool>;
    pub fn node_id_fixture_equals(&self, expected: &[u8]) -> anyhow::Result<bool>;
    pub fn seed_node_id_fixture(&self, contents: &[u8]) -> anyhow::Result<()>;
    pub fn swap_root_fixture(&self) -> anyhow::Result<IsolatedNodeRootSwap>;
    pub fn symlink_node_id_fixture(&self, target: &std::path::Path) -> anyhow::Result<()>;
}
/// What this node can do
pub struct NodeCapabilities { … }
impl NodeCapabilities {
    /// Describe the current host using model availability supplied by the composition root.
    pub fn for_current_host(ram_gb: usize, local_model: Option<String>, has_teacher_api: bool) -> Self;
    pub fn is_cloud_only(&self) -> bool;
}
/// A finch node's stable identity
pub struct NodeIdentity { … }
impl NodeIdentity {
    /// Generate a deterministic UUID v5 for a device fingerprint string.
    pub fn device_uuid(fingerprint: &str) -> Uuid;
    /// Load existing identity or create one on first run.
    pub fn load_or_create() -> Result<Self>;
    /// Short display prefix (first 8 chars of UUID)
    pub fn short_id(&self) -> String;
}
/// Full description of this node's capabilities.
pub struct NodeInfo { … }
impl NodeInfo {
    pub fn load(capabilities: NodeCapabilities) -> anyhow::Result<Self>;
    /// One-line summary for status display
    pub fn summary(&self) -> String;
}
/// Persistent cryptographic identity for authenticating Finch transports.
pub struct NodeSigningIdentity { … }
impl NodeSigningIdentity {
    pub fn from_secret(secret: [u8; 32]) -> Self;
    pub fn load_or_create(state_directory: &Path) -> Result<Self>;
    pub fn public_key_bytes(&self) -> [u8; 32];
    pub fn sign(&self, message: &[u8]) -> [u8; 64];
    pub fn verify(public_key: [u8; 32], message: &[u8], signature: [u8; 64]) -> Result<()>;
}
pub struct NodeTlsIdentity { … }
impl NodeTlsIdentity {
    pub fn certificate_der(&self) -> &[u8];
    pub fn from_signing_identity(identity: &NodeSigningIdentity, hostname: &str) -> Result<Self>;
}
/// Aggregate statistics for this node's work
pub struct WorkStats { … }
impl WorkStats {
    /// Average response latency in milliseconds
    pub fn avg_latency_ms(&self) -> f64;
    /// Local model usage percentage
    pub fn local_pct(&self) -> f64;
    pub fn new() -> Self;
}
/// Thread-safe work statistics tracker
pub struct WorkTracker { … }
impl WorkTracker {
    /// Load previously persisted stats (for cumulative totals across restarts)
    pub fn load_persisted() -> Result<WorkStats>;
    pub fn new() -> Arc<Self>;
    /// Save snapshot to ~/.finch/work_stats.json
    pub fn persist(&self) -> Result<()>;
    /// Record a completed query
    pub fn record_query(&self, latency_ms: u64, used_local: bool);
    /// Snapshot current stats
    pub fn snapshot(&self) -> WorkStats;
}
```

## Functions

```rust
/// Collect basic machine specs for registry metadata.
pub fn collect_machine_specs() -> (u32, u64, u64) { … }
/// Install Finch's process-wide Rustls provider, or reject an incompatible provider that another TLS consumer installed first.
pub fn install_crypto_provider() -> Result<()> { … }
```
