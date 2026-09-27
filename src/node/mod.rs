//! Compatibility facade for the extracted `finch-node` crate.

pub use finch_node::{
    install_crypto_provider, NodeCapabilities, NodeIdentity, NodeInfo, NodeSigningIdentity,
    NodeTlsIdentity, WorkStats, WorkTracker,
};
#[cfg(unix)]
pub use finch_node::{IsolatedNodeRootSwap, IsolatedNodeTestState};
