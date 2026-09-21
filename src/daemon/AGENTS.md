# Daemon subsystem

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/daemon` owns process lifecycle, auto-spawn, the bounded rotating log, and upgrade
preflight for the background Finch server. It is root application composition, not part
of the `finch-brain` crate. Its public contract is the facade in [`mod.rs`](mod.rs);
callers must not name child modules.

## Boundary

- The [README](README.md) traces the daemon process and client auto-spawn workflows.
  Callers outside this directory use `crate::daemon::Item` or `finch::daemon::Item`.
- `lifecycle.rs`, `log.rs`, `spawn.rs`, and `upgrade.rs` are private implementation
  modules. Add public surface with a demonstrated caller and a flat re-export in `mod.rs`;
  do not regenerate a signature catalog.
- Must not own Brain storage, HTTP routes, IPC protocol, or model loading.
- Do not change spawn, lifecycle, log-rotation, or upgrade-preflight behavior in a
  facade commit. This application-bound module is not a mechanical `finch-daemon` crate cut.

**Dependencies and direction:** `spawn` uses config, startup, IPC compatibility, and the
Brain test-isolation authority; `upgrade` composes client, server, and Brain proof paths.
Those application edges are why this module stays at the root. Lower-level Brain and IPC
crates must not import daemon lifecycle or log policy.

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

Run through the repository supervisor:

```bash
./scripts/test_brains.sh cargo test --lib daemon::
```

Use the smallest matching filter first. Run `python3 scripts/check_docs.py` and
`python3 scripts/check_facade_boundaries.py` after capsule or facade changes.
