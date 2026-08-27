/// SSH client module.
///
/// `SshSessionStore` holds live connections keyed by UUID.
/// Host adapters may represent an SSH session as an opaque handle into this store.
///
/// The russh crate provides async SSHv2 over tokio.  All operations
/// yield back to the tokio scheduler at every `.await`, giving the
/// event loop a chance to process other events between SSH I/O steps.
pub mod client;

pub use client::{HostKeyPolicy, SshSession, SshSessionStore};
