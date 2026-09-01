//! Cap'n Proto IPC layer — CLI ↔ daemon over Unix domain socket.
//!
//! ## Architecture
//!
//! ```text
//! CLI process                           Daemon process
//! ─────────────────────────────────────────────────────
//! IpcClient                             IpcServer
//!   │                                      │
//!   │  capnp-rpc over UnixStream           │
//!   └──────── ~/.finch/daemon.sock ────────┘
//! ```
//!
//! The HTTP server on port 11435 stays up for external OpenAI-compatible
//! clients (VS Code / Continue.dev).  This module is the internal fast path.

pub(crate) mod brain_codec;
pub(crate) mod checkpoint_codec;
pub mod client;
pub mod events;
pub mod schema;
pub mod server;
pub mod transport;

pub use client::IpcClient;
pub use events::{EventBus, QueuedEvent};
pub use server::start_ipc_server;
pub use transport::DAEMON_SOCK_PATH;

/// Compatibility generation for the frontend/daemon Cap'n Proto contract.
/// Increment this whenever a change requires both processes to come from the
/// same build generation. Older daemons leave the added ping field at zero,
/// so new frontends fail before acquiring Brain identities or callbacks.
pub const IPC_PROTOCOL_VERSION: u32 = 10;
