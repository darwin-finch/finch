# Brain run lifecycle

A Brain run is a durable unit of work with its own identity and status. This module owns the
run/lease/handoff vocabulary, the allowed status transitions, and the small durable intent
record used when a runner disconnects. It does not execute a program, decide server policy,
or write the Brain event journal. `BrainStore` combines these rules with the journal.

Two callers show the boundary:

1. [BrainStore](../store.rs) starts a run with a new `RunId`, validates each status change,
   and journals the result. On a runner disconnect it writes a terminalization intent before
   changing durable run state, then clears that intent once the state is recorded. This module
   defines the transition and intent rules; the store owns sequencing and recovery.
2. The [server Brain service](../../../../src/server/brain_service.rs) calls `BrainStore`
   to start, inspect, cancel, and transition runs. It gives application requests a `BrainRun`
   and `BrainRunStatus` via the flat `finch_brain` facade, while the server keeps runner dispatch,
   authorization, and HTTP/IPC request policy. The run module does not depend back on the server.

Read [AGENTS.md](AGENTS.md) for dependency and lifetime rules, [`mod.rs`](mod.rs) for the
internal Brain seam, and the [crate facade](../../lib.rs) for the external callable surface.
Rustdoc supplies signatures without a checked-in symbol catalog.
