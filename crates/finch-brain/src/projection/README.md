# Read-only Brain views

This module turns durable Brain facts into snapshots, wire messages, and lightweight
directory-listing summaries. It owns the shape of a Brain snapshot and the observer-safe view
of effect-audit events. It does not append journal events, acquire runner authority, hydrate
Brains merely to list them, or decide what a terminal renders.

Two callers show the boundary:

1. [`BrainStore::list_summaries_unhydrated`](../store.rs) uses this module to scan existing
   metadata and journal files for each named Brain without loading the reducer or creating
   files. The store chooses which Brain names to list; the projection supplies the read-only
   summary of runs, connected participants, runner status, revision, and bytes.
2. The [REPL Brain handler](../../../../src/cli/repl_event/brain_handler.rs) inspects a
   `BrainSnapshot` after a lease-renewal error. Its handoff query distinguishes a deliberate
   durable transfer from an ordinary expiry, so the old runner identity is not silently
   re-minted. The REPL owns reconnection and status display; this module owns the snapshot
   fact derived from journal events.

The [agent contract](AGENTS.md) states read-only and dependency rules. [`mod.rs`](mod.rs)
is the nested facade; external crates use the flat [`finch-brain` facade](../lib.rs).
