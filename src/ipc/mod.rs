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
/// Generation 10 carries named-Brain Prompt mention snapshots (path, digest,
/// content) on Cap'n Proto submit and event Prompt.
pub const IPC_PROTOCOL_VERSION: u32 = 10;

/// Short package identity advertised on `/health` and IPC ping.
pub fn package_identity() -> &'static str {
    concat!("finch ", env!("CARGO_PKG_VERSION"))
}

/// Protocol generation advertised by a running daemon's `/health` document.
///
/// Older daemons omit the field. Missing or unreadable values stay at 0 so a
/// newer frontend fails closed instead of treating HTTP 200 as compatibility.
pub fn protocol_generation_from_health_json(value: &serde_json::Value) -> u32 {
    value
        .get("protocol_generation")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32
}

/// Uptime from a `/health` document, or 0 when the field is absent.
pub fn uptime_seconds_from_health_json(value: &serde_json::Value) -> u64 {
    value
        .get("uptime_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// One human leftover-daemon error naming both generations and the kick command.
pub fn leftover_daemon_message(
    frontend_generation: u32,
    daemon_generation: u32,
    uptime_seconds: Option<u64>,
) -> String {
    let uptime = uptime_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| format!(" (up for {})", format_uptime(seconds)))
        .unwrap_or_default();
    format!(
        "This Finch speaks protocol {frontend_generation}; the running daemon speaks {daemon_generation}{uptime}. Stop it with: finch daemon-stop"
    )
}

fn format_uptime(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if seconds >= DAY {
        let days = seconds / DAY;
        let hours = (seconds % DAY) / HOUR;
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        }
    } else if seconds >= HOUR {
        let hours = seconds / HOUR;
        let minutes = (seconds % HOUR) / MINUTE;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else if seconds >= MINUTE {
        format!("{}m", seconds / MINUTE)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod health_advertisement_tests {
    use super::*;

    #[test]
    fn omitted_health_protocol_generation_fails_closed_at_zero() {
        let body = serde_json::json!({
            "status": "healthy",
            "uptime_seconds": 7200,
            "named_brains": 1
        });
        assert_eq!(
            protocol_generation_from_health_json(&body),
            0,
            "older daemons omit protocol_generation; HTTP 200 must not be treated as compatibility. body={body}"
        );
        assert_eq!(uptime_seconds_from_health_json(&body), 7200);
        let message = leftover_daemon_message(IPC_PROTOCOL_VERSION, 0, Some(7200));
        assert!(
            message.contains(&format!("protocol {IPC_PROTOCOL_VERSION}")),
            "leftover error must name this Finch generation; message={message}"
        );
        assert!(
            message.contains("speaks 0"),
            "leftover error must name the running daemon generation; message={message}"
        );
        assert!(
            message.contains("up for 2h"),
            "leftover error must include uptime; message={message}"
        );
        assert!(
            message.contains("finch daemon-stop"),
            "leftover error must name the exact kick command; message={message}"
        );
    }

    #[test]
    fn advertised_health_protocol_generation_is_read_verbatim() {
        let body = serde_json::json!({
            "protocol_generation": IPC_PROTOCOL_VERSION,
            "uptime_seconds": 12,
            "package_identity": package_identity(),
        });
        assert_eq!(
            protocol_generation_from_health_json(&body),
            IPC_PROTOCOL_VERSION
        );
    }
}

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
