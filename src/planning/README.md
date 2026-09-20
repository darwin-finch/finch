# planning: IMPCPD iterative plan refinement

Owns the `/plan` command's IMPCPD loop: iterative plan critique, persona selection, and the
embedded methodology spec. It exists as its own facade — separate from the providers/CLI it
depends on — because plan generation, critique, and steering are one cohesive algorithm that
should change together, not be threaded through call sites that just need a plan.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
