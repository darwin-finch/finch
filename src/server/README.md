# Daemon-facing server

`server` assembles Finch's HTTP routes and daemon-side IPC adapter around shared named-Brain
state, provider/model services, runner callbacks, authentication, and rate limits. It is root
application composition: it does not own the Brain journal, domain-neutral IPC protocol,
program runtime, or daemon process lock and log.

Two callers show the boundary:

1. [`run_daemon` in the executable](../main.rs) constructs `AgentServer` with the provider
   graph, router, model state, metrics, and configuration. It then serves the HTTP router and
   starts `start_ipc_server` against the same shared server instance. Process ownership and
   log setup happen in `src/daemon` and the executable; this module owns listener and request
   handling after composition.
2. The [IPC client adapter](../client/ipc.rs) uses server-owned runner request and result
   contracts to bridge a local runner to daemon-side named-Brain turns. The server brokers
   approvals, effect reservations, and turn completion; the client owns its connection and
   runner execution. Shared wire framing itself belongs to `finch-ipc`.

Read [AGENTS.md](AGENTS.md) for dependency and lifecycle rules, and [`mod.rs`](mod.rs) for the
callable facade. A few public methods return types not yet exported by that facade, and one
crate-visible IPC child path remains; [#1050](https://github.com/darwin-finch/finch/issues/1050)
tracks those API gaps separately from this documentation slice.
