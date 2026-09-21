# Brain event journal

The named Brain's canonical history is an append-only event log. This module owns the event
envelope, mutation receipt, persistent JSONL framing, and recovery after a torn or corrupt
tail. A physical record may be one event or a checksummed batch, but replay exposes only
committed logical events. It does not decide whether a mutation is authorized, schedule work,
or turn events into a terminal display.

Two callers show the boundary:

1. [`BrainStore`](../store.rs) validates a mutation against its current Brain state, checks an
   exact receipt for replay, then appends the accepted event or batch. The first canonical
   event carries the receipt, so a retry can return the same accepted outcome without a
   second transition. The store owns authorization and state locks; the journal owns durable
   commit and readback.
2. [Read-only Brain listing](../projection/mod.rs) asks `scan_readonly` for event, revision,
   runner, attachment, and run facts from `events.jsonl` without hydrating a Brain or writing
   files. Projection decides which facts are safe to display; the journal supplies the
   read-only on-disk scan, including its torn-tail handling.

The [agent contract](AGENTS.md) states framing and dependency rules. [`mod.rs`](mod.rs) is the
nested facade; external crates use the flat [`finch-brain` facade](../lib.rs), not this path.
