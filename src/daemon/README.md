# daemon: background process lifecycle

Owns process lifecycle, auto-spawn, the bounded rotating log, and upgrade preflight for the
background Finch server. It exists as its own facade — even though Brain owns this directory
conceptually — because process spawn/lifecycle/log-rotation code has sharp edges (stale sockets,
leftover PIDs, log growth) that deserve isolation from Brain's storage and scheduling concerns.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).
