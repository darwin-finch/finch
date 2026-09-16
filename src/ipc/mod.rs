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
mod codec;
pub mod events;
pub mod schema;
pub mod server;
pub mod transport;

pub use client::IpcClient;
pub(crate) use codec::{
    brain_remote_command_fingerprint, decode_brain_remote_envelope, decode_checkpoint_bytes,
    encode_brain_remote_envelope, encode_checkpoint_bytes,
    encode_runtime_application_message_packed, BrainRemoteCommand, BrainRemoteCommandKind,
    BrainRemoteEnvelope, BrainRemoteMutation, BrainRemoteReply,
};
pub use events::{EventBus, QueuedEvent};
pub use server::start_ipc_server;
pub use transport::DAEMON_SOCK_PATH;

/// Compatibility generation for the frontend/daemon Cap'n Proto contract.
/// Increment this whenever a change requires both processes to come from the
/// same build generation. Older daemons leave the added ping field at zero,
/// so new frontends fail before acquiring Brain identities or callbacks.
/// Generation 9 requires packed `RuntimeApplicationMessage` envelopes on
/// runner-result delivery and exposes `pendingDelivery` / cursor ack.
pub const IPC_PROTOCOL_VERSION: u32 = 9;

#[cfg(test)]
mod codec_boundary_tests {
    use super::{
        decode_brain_remote_envelope, decode_checkpoint_bytes, encode_brain_remote_envelope,
        encode_checkpoint_bytes, BrainRemoteCommand, BrainRemoteCommandKind, BrainRemoteEnvelope,
    };
    use crate::vm::TypedRuntimeCheckpoint;
    use std::collections::BTreeMap;

    /// Codec types are reached through the ipc facade, never through
    /// `brain_codec`, `checkpoint_codec`, or the private `codec` child.
    #[test]
    fn test_codec_types_are_reached_through_the_ipc_facade() {
        let mut hits = Vec::new();
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ipc_root = manifest.join("src/ipc");
        let codec_root = ipc_root.join("codec");
        for tree in ["src", "tests"] {
            collect_stale_codec_paths(
                &manifest.join(tree),
                &ipc_root,
                &codec_root,
                &manifest.join("src/ipc/mod.rs"),
                &mut hits,
            );
        }
        assert!(
            hits.is_empty(),
            "codec types must be used via crate::ipc, not ipc::brain_codec, ipc::checkpoint_codec, or ipc::codec from outside ipc; found: {hits:?}"
        );
    }

    fn collect_stale_codec_paths(
        dir: &std::path::Path,
        ipc_root: &std::path::Path,
        codec_root: &std::path::Path,
        facade: &std::path::Path,
        hits: &mut Vec<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == *facade {
                continue;
            }
            if path.is_dir() {
                collect_stale_codec_paths(&path, ipc_root, codec_root, facade, hits);
                continue;
            }
            let Some(name) = path.to_str() else {
                continue;
            };
            if !name.ends_with(".rs") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let under_codec = path.starts_with(codec_root);
            let under_ipc = path.starts_with(ipc_root);
            for (line_number, line) in contents.lines().enumerate() {
                let names_old_module =
                    line.contains("ipc::brain_codec") || line.contains("ipc::checkpoint_codec");
                let names_codec_child = line.contains("ipc::codec::");
                let stale = (names_old_module && !under_codec) || (names_codec_child && !under_ipc);
                if stale {
                    hits.push(format!("{}:{}: {}", name, line_number + 1, line.trim()));
                }
            }
        }
    }

    /// A Detach remote envelope keeps a pinned Cap'n Proto frame so a facade
    /// move cannot silently change Brain collaboration bytes.
    #[test]
    fn test_detach_remote_envelope_bytes_are_stable() {
        let envelope = BrainRemoteEnvelope::Command(BrainRemoteCommand {
            request_id: 1,
            mutation: None,
            kind: BrainRemoteCommandKind::Detach,
        });
        let encoded =
            encode_brain_remote_envelope(&envelope).expect("Detach remote envelope must encode");
        assert_eq!(
            hex::encode(&encoded),
            "000000000800000000000000010001000100000000000000000000000300020001000000000000000200000000000000000000000000000000000000000000000000000000000000",
            "Brain remote Detach framing must stay byte-stable; encoded={encoded:?}"
        );
        let decoded =
            decode_brain_remote_envelope(&encoded).expect("pinned Detach bytes must decode");
        assert_eq!(
            decoded, envelope,
            "pinned Detach bytes must round-trip the same command"
        );
    }

    /// An empty typed checkpoint keeps a pinned Cap'n Proto frame so durable
    /// Brain runtime snapshots cannot change encoding in a facade commit.
    #[test]
    fn test_empty_checkpoint_bytes_are_stable() {
        let checkpoint = TypedRuntimeCheckpoint {
            version: 1,
            stack: Vec::new(),
            functions: BTreeMap::new(),
            producer_fibers: BTreeMap::new(),
        };
        let encoded = encode_checkpoint_bytes(&checkpoint).expect("empty checkpoint must encode");
        assert_eq!(
            hex::encode(&encoded),
            "000000000800000000000000010003000100000000000000090000000700000009000000070000000900000007000000000000000200010000000000000002000000000000000200",
            "empty typed checkpoint framing must stay byte-stable; encoded={encoded:?}"
        );
        let decoded =
            decode_checkpoint_bytes(&encoded).expect("pinned empty checkpoint bytes must decode");
        assert_eq!(
            decoded, checkpoint,
            "pinned empty checkpoint bytes must round-trip"
        );
    }
}
