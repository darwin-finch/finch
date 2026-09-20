//! Domain-neutral Cap'n Proto transport primitives shared by Finch clients and servers.

// Cap'n Proto generated code must live at the crate root so that the
// self-references emitted by capnpc (`crate::finch_ipc_capnp::…`) resolve.
#[allow(
    clippy::all,
    dead_code,
    unused_imports,
    unused_parens,
    non_camel_case_types,
    non_snake_case
)]
pub mod finch_ipc_capnp {
    include!(concat!(env!("OUT_DIR"), "/finch_ipc_capnp.rs"));
}

mod events;
mod transport;
mod value_codec;

pub use events::{EventBus, QueuedEvent};
pub use transport::{sock_path, DAEMON_SOCK_PATH};
pub use value_codec::{decode_json_value, encode_json_value};

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
mod tests {
    use super::*;

    #[test]
    fn omitted_health_protocol_generation_fails_closed_at_zero() {
        let body = serde_json::json!({
            "status": "healthy",
            "uptime_seconds": 7200,
            "named_brains": 1
        });
        assert_eq!(protocol_generation_from_health_json(&body), 0);
        assert_eq!(uptime_seconds_from_health_json(&body), 7200);
        let message = leftover_daemon_message(IPC_PROTOCOL_VERSION, 0, Some(7200));
        assert!(message.contains(&format!("protocol {IPC_PROTOCOL_VERSION}")));
        assert!(message.contains("speaks 0"));
        assert!(message.contains("up for 2h"));
        assert!(message.contains("finch daemon-stop"));
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
