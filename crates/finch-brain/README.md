# Finch Brain

A Brain is a durable, named agent whose runs, schedule, attachments, and authority survive a
daemon restart. This crate owns the Brain's event-backed state, identity and credential rules,
and Brain-specific local and remote client semantics. It does not own the HTTP server, REPL
presentation, daemon process, domain-neutral IPC framing, or typed program execution.

Two callers make the boundary concrete:

1. The [server's Brain lifecycle service](../../src/server/brain_service.rs) takes an application
   request, serializes creation under a Brain execution lock, and calls `BrainStore::snapshot` to
   establish the Brain. For runs, it calls `BrainStore::start_run_with_parent`; for attachments,
   `BrainStore::attach`. The server owns HTTP authentication, request routing, and pending
   connection expiry. The store owns durable records, Brain identity, and run/attachment state.
2. The [REPL Brain handler](../../src/cli/repl_event/event_loop/brain.rs) chooses a local IPC
   attachment or an invitation-backed remote one, then builds `AttachedBrainClient` with
   `RemoteBrainClient` where needed. The REPL owns the user's selection and status display; the
   client owns Brain-specific protocol operations, not the canonical Brain record.

Start at the [flat crate facade](src/lib.rs); rustdoc (`cargo doc -p finch-brain --no-deps --open`)
shows methods on its re-exported types without requiring private implementation files. The
[agent contract](AGENTS.md) gives dependency and lifetime rules. The
[Brain test inventory](../../tests/BRAIN_TEST_INVENTORY.md) maps the focused proofs.
