# Brain schedules and due work

This module represents scheduled Brain work and calculates which occurrences are due. It
owns delivery-policy arithmetic, a stable due index, and the immutable program snapshot in
each due event. It does not dispatch a runner or make a schedule durable on its own: the Brain
store journals events, and the application decides when to poll and run them.

Two callers show the boundary:

1. [BrainStore](../store.rs) validates and records schedules, maintains the due index, and
   selects due work without hydrating every Brain. It uses this module's due-window rules to
   attach a queued `BrainRun` and a source/effect snapshot to each journaled delivery. The
   store owns atomic event append and restart recovery; this module owns calculation and data
   meaning.
2. The [server Brain service](../../../../src/server/brain_service.rs) accepts application
   schedule requests and returns `BrainSchedule`/`ScheduleId` through the flat `finch_brain`
   facade. It asks `BrainStore` to create, inspect, or retire a schedule; runner dispatch and
   authorization remain with the server. The REPL then uses those returned identities to
   display and cancel scheduled work, without reaching into the due index.

Read [AGENTS.md](AGENTS.md) for dependency and lifetime rules, [`mod.rs`](mod.rs) for the
internal Brain seam, and the [crate facade](../../lib.rs) for the external callable surface.
Rustdoc supplies signatures without a checked-in symbol catalog.
