# Daemon process boundary

This module owns the background Finch process's lifecycle, auto-spawn policy, bounded logs,
and staged-upgrade preflight. It composes application services, so it stays in the root
package. It does not implement HTTP routes, Brain persistence, IPC wire rules, or local-model
loading.

Two callers show the boundary:

1. [`run_daemon` in the executable](../main.rs) opens a rotating log, then acquires
   `DaemonLifecycle`'s process-lifetime instance guard before binding server transports.
   The detached-child marker alone permits taking over stdout/stderr; foreground daemon and
   worker modes retain their operator-visible output. The executable owns server, provider,
   and model composition after this lifecycle gate.
2. [`DaemonClient`](../client/daemon_client.rs) calls `ensure_daemon_running` when auto-spawn is
   enabled, including a bounded retry after a connection failure. The daemon module probes
   health and protocol compatibility before reusing or spawning a process; the client owns
   request retries and whether auto-spawn is enabled.

Read [AGENTS.md](AGENTS.md) for isolation, log, and socket invariants, and [`mod.rs`](mod.rs)
for the flat callable facade. The application-level upgrade preflight remains here because
it coordinates client, server, and Brain proof paths rather than adding a reverse dependency
to the lower-level crates.
