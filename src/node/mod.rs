//! Compatibility facade for the extracted `finch-node` crate.

pub use finch_node::{
    collect_machine_specs, install_crypto_provider, NodeCapabilities, NodeIdentity, NodeInfo,
    NodeSigningIdentity, NodeTlsIdentity, WorkStats, WorkTracker,
};
#[cfg(unix)]
pub use finch_node::{IsolatedNodeRootSwap, IsolatedNodeTestState};
