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

pub mod client;
pub mod schema;
pub mod server;
pub mod transport;

pub use client::IpcClient;
pub use server::start_ipc_server;
pub use transport::DAEMON_SOCK_PATH;
