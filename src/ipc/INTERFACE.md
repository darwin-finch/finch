# ipc — public interface

Generated from [`src/ipc/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/ipc/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Named async event bus with continuation support.
pub struct EventBus { … }
impl EventBus {
    /// Process all currently queued events (non-blocking once the queue is empty).
    pub async fn flush(&mut self);
    /// Drain the queue until it is empty or the channel is closed.
    pub async fn run(&mut self);
    pub fn new() -> Self;
    /// Register an async handler for events with the given name.
    pub fn register<F, Fut>(&mut self, name: impl Into<String>, handler: F) where F: Fn(QueuedEvent) -> Fut + Send + Sync + 'static, Fut: Future<Output = Option<QueuedEvent>> + Send + 'static,;
    /// Enqueue an event.
    pub fn send(&self, event: QueuedEvent);
    /// Return a sender that can enqueue events from other tasks.
    pub fn sender(&self) -> mpsc::UnboundedSender<QueuedEvent>;
}
/// A single event on the bus.
pub struct QueuedEvent { … }
impl QueuedEvent {
    /// Produce a continuation event: same `id`, new `name` and `payload`.
    pub fn continue_as(&self, name: impl Into<String>, payload: serde_json::Value) -> Self;
    pub fn new(name: impl Into<String>, payload: serde_json::Value) -> Self;
}
```

## Functions

```rust
pub fn decode_json_value(reader: finch_ipc_capnp::json_value::Reader<'_>) -> anyhow::Result<serde_json::Value> { … }
pub fn encode_json_value(builder: finch_ipc_capnp::json_value::Builder<'_>, value: &serde_json::Value) -> anyhow::Result<()> { … }
/// One human leftover-daemon error naming both generations and the kick command.
pub fn leftover_daemon_message(frontend_generation: u32, daemon_generation: u32, uptime_seconds: Option<u64>) -> String { … }
/// Short package identity advertised on `/health` and IPC ping.
pub fn package_identity() -> &'static str { … }
/// Protocol generation advertised by a running daemon's `/health` document.
pub fn protocol_generation_from_health_json(value: &serde_json::Value) -> u32 { … }
/// Expand `~/` prefix in a socket path.
pub fn sock_path() -> PathBuf { … }
/// Uptime from a `/health` document, or 0 when the field is absent.
pub fn uptime_seconds_from_health_json(value: &serde_json::Value) -> u64 { … }
```

## Constants

```rust
/// Default path for the IPC Unix domain socket.
pub const DAEMON_SOCK_PATH: &str = "~/.finch/daemon.sock";
/// Compatibility generation for the frontend/daemon Cap'n Proto contract.
pub const IPC_PROTOCOL_VERSION: u32 = 10;
```

## Modules

```rust
/// Re-exported from `lib`.
pub mod finch_ipc_capnp { … }
```
