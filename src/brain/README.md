# Brain: durable named agents

A "Brain" is Finch's durable, named, resumable agent — the mechanism behind `finch attach
<brain-name>`. This directory owns the store composition, credential authority, remote Brain
clients, and the long-lived background-process task table. It exists so an agent's state (what it
ran, what's scheduled, who's attached) survives process restarts, disconnects, and reconnects
without losing exactly-once guarantees on run completion.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md); persistence
and coordination internals live in nested facades — [`journal`](journal/README.md),
[`schedule`](schedule/README.md), [`run`](run/README.md), [`attachment`](attachment/README.md),
and [`projection`](projection/README.md).

## Further documentation

[Brain test inventory](../../tests/BRAIN_TEST_INVENTORY.md) — what the Brain test suite covers.
