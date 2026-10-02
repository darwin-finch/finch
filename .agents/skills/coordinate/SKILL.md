---
name: coordinate
description: Coordinate sibling workstreams with solution contracts, exclusive work claims, isolated worktrees, and task packets. Use when running parallel workers, locking file or semantic scope, writing an implementation contract, or integrating sibling changes. Does not pick the next backlog item or land. Use when the user runs /coordinate.
---

# Coordination

Deliver the smallest coherent change that satisfies the accepted outcome while keeping independently evolving subsystems understandable in bounded context. This workflow is model-neutral: [packets name a lane](../../references/task-packet.md), not a vendor model or agent type; use the runtime's available delegation and workspace-isolation facilities.

Load the tracker from [`.agents/binding`](../../references/load-binding.md) before any tracker verb. Skip ordinary research and lightweight prose edits. A single backlog item with no siblings goes to `$implement`; load this when two or more workstreams, or an integration owner distinct from the workers, are in play. `$backlog-loop` owns the replenishing pool across tickets. This skill owns siblings of one parent outcome.

## Establish the work boundary

Read `AGENTS.md` and only the design or subsystem material relevant to the requested outcome. Before production code, record a compact [solution contract](../../references/solution-contract.md). A direct, well-specified request may serve as most of that contract; do not invent tracker ceremony.

Do not begin implementation until the outcome, non-goals, affected authority, acceptance evidence, repository, exact base revision, and dependencies are sufficiently explicit. When the durable record changes, the contract includes the [cost-of-reversal](../../references/engineering-judgment.md) answers. New product decisions go through the responsible human; a worker or test result cannot manufacture authority.

Record ownership with the [work-claim protocol](../../references/work-claims.md) mapped by the loaded binding.

## Decompose for bounded context

Use [parallel coordination](../../references/parallel-coordination.md) when two or more independently testable workstreams can proceed without concurrent ownership of the same files or semantic boundary. Fan out to `$implement` workers (one worktree each), then join before `$review`. How many at once is this machine and this repo's tests, same ceiling as `$backlog-loop`. Keep work local when decomposition would add more interface negotiation than useful isolation.

The coordinator owns decomposition, shared interface decisions, dependency order, collision checks, integration, and completion evidence. Each delegated worker receives a [task packet](../../references/task-packet.md) with exclusive scope and returns a narrow handoff. Workers must not silently expand across another worker's subsystem or integrate sibling work themselves unless their packet grants that role. Before calling the native delegation mechanism for the active harness, load `.agents/harnesses/<active harness>.md` and resolve each packet's lane through it — a lane written into a packet and never resolved has no effect.

Prefer cohesive subsystems with narrow facades, private internals, explicit dependency direction, co-located tests, and boundary-level integration proof. Do not create generic abstractions merely to manufacture parallel tasks.

## Integrate siblings, then hand off to review

Workers implement via `$implement`. This skill does not write the feature and does not land.

Load Finch's [execution adapter](../../references/finch-execution.md) for combined-candidate gates,
Cargo isolation, and generated-output cleanup.

The integration owner verifies interface compatibility, combines handoffs in dependency order into a clean workspace without merge commits, resolves conflicts with the relevant subsystem owner, and runs affected unit and boundary tests on the combined revision. Worker commits are transport artifacts: squash each accepted feature or fix into its own commit and apply those commits linearly. The combined tip is the candidate. `$review` is the only skill that lands it. Parallel workers finishing their local scopes is not completion by itself.

Handing the candidate to review does not end coordination. After `$review` lands it, the coordinator completes the [post-integration synchronization and cleanup](../../references/work-claims.md#post-integration-synchronization-and-cleanup) before reporting the parent outcome complete. If review does not land the candidate, preserve or release each workspace according to its recorded next action; do not discard recoverable work.

Cleanup is part of the outcome, not deferred housekeeping. Retire each owned worker, probe, review,
and integration worktree as soon as its disposition is durable. For Finch, remove the registered
worktree first, then inventory and reclaim its orphaned Cargo target namespace through the
[execution adapter](../../references/finch-execution.md). A deliberately retained workspace or
cache needs a named owner and removal trigger. Never delete another coordinator's workspace or a
cache whose ownership or live-process state cannot be proved.
