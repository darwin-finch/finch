# Daemon subsystem

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/daemon` owns process lifecycle, auto-spawn, the bounded rotating log, and upgrade
preflight for the background Finch server. Brain owns this directory; it is not a
separate layer. Its public contract is the facade in `mod.rs`; callers must not name
child modules.

## Boundary

- Callers outside this directory use `crate::daemon::Item`. The one production incoming
  edge is `client` (`ensure_daemon_running`); `main` composes lifecycle, spawn, and log
  at the daemon process boundary.
- `lifecycle.rs`, `log.rs`, `spawn.rs`, and `upgrade.rs` are private implementation
  modules. Add public surface by re-exporting it from `mod.rs`.
- Must not own Brain storage, HTTP routes, IPC protocol, or model loading.
- Do not change spawn, lifecycle, log-rotation, or upgrade-preflight behavior in a
  facade commit. Do not extract `finch-daemon`.

## Invariants

- Isolated Brain tests disable daemon discovery, reuse, and auto-spawn.
- Detached children are the only processes that take over stdout/stderr via
  `DETACHED_DAEMON_ENV`.
- The persistent log is size-bounded, owner-only, and never follows a symlink.
- A live IPC listener is never reported or reaped as crash leftovers;
  `has_stale_files`, `ipc_listener_alive`, and `stop_daemon` share one
  connect-based live-vs-stale socket probe (`lifecycle.rs`), never existence
  alone.

## Focused tests

Run through the repository supervisor with a worktree-specific absolute Cargo target directory:

```bash
./scripts/test_brains.sh cargo test --lib daemon::
```

Use the smallest matching filter first. `mod.rs`'s re-exports are the whole public surface — read
it directly for exact signatures.
