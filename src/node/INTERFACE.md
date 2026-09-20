# node — public interface

Generated from [`src/node/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/node/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Re-exported from `finch-node`.
#[cfg(unix)]
pub struct IsolatedNodeRootSwap { … }
/// Opaque disposable state owned by worker-node integration tests. Re-exported from `finch-node`.
#[cfg(unix)]
pub struct IsolatedNodeTestState { … }
/// What this node can do Re-exported from `finch-node`.
pub struct NodeCapabilities { … }
/// A finch node's stable identity Re-exported from `finch-node`.
pub struct NodeIdentity { … }
/// Full description of this node's capabilities. Re-exported from `finch-node`.
pub struct NodeInfo { … }
/// Persistent cryptographic identity for authenticating Finch transports. Re-exported from `finch-node`.
pub struct NodeSigningIdentity { … }
/// Re-exported from `finch-node`.
pub struct NodeTlsIdentity { … }
/// Aggregate statistics for this node's work Re-exported from `finch-node`.
pub struct WorkStats { … }
/// Thread-safe work statistics tracker Re-exported from `finch-node`.
pub struct WorkTracker { … }
```

## Functions

```rust
/// Collect basic machine specs for registry metadata. Re-exported from `finch-node`.
pub fn collect_machine_specs() -> (u32, u64, u64) { … }
/// Install Finch's process-wide Rustls provider, or reject an incompatible provider that another TLS consumer installed first. Re-exported from `finch-node`.
pub fn install_crypto_provider() -> Result<()> { … }
```
