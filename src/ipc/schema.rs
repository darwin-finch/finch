//! Re-exports the Cap'n Proto generated code from the crate root.
//!
//! The generated code (`finch_ipc_capnp.rs`) is included at the crate root
//! (`lib.rs`) so that capnpc's self-referential paths (`crate::finch_ipc_capnp::…`)
//! resolve correctly.  This module exposes it under `ipc::schema::finch_ipc_capnp`
//! for callers that prefer a more qualified import path.

// Re-export from the crate root where the generated code lives.
pub use crate::finch_ipc_capnp;
