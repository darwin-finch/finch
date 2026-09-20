# runtime: program runtime service and host effects

Owns typed program execution, capability/authority binding, host-effect audit, the automation
broker, and the child-agent vocabulary — the layer that turns a compiled program into something
that can actually touch the host (files, tools, network) under an authorized capability grant. It
exists between `finch-vm` (which only executes verified IR) and `brain` (which schedules and
authorizes runs), so execution mechanics and host-effect policy have one owner. The
Runtime/Application ABI here is a development seam, not a frozen production compatibility promise.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).
