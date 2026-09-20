# brain/schedule: due-work selection

Owns schedules and the store-wide due index the daemon uses to decide which Brain wakes up next,
and when. It exists as its own facade so due-window arithmetic and coalesce/catch-up semantics
(what happens when a scheduled run was missed) have one authoritative implementation instead of
being reimplemented at each caller.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
