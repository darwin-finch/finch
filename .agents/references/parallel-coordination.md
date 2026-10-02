# Parallel coordination

Use parallel workers to reduce context and elapsed time while retaining one accountable integration path. Load this from [coordinate](../skills/coordinate/SKILL.md) and [backlog-loop](../skills/backlog-loop/SKILL.md).

## Decomposition test

Delegate a workstream only when it has:

- one observable outcome;
- a cohesive subsystem or verification perspective;
- files and semantic authority that do not overlap another active worker;
- a stable input/output interface or an explicit dependency on the worker defining it;
- independent tests or evidence;
- an identified integration owner.

Keep coupled edits together when workers would repeatedly negotiate the same type, migration, state transition, generated file, or user flow. Research and verification can often run independently even when production edits cannot. Do not create generic abstractions merely to manufacture parallel tasks.

## Coordination topology

Use one coordinator for the parent outcome. The coordinator maintains the dependency graph, assigns exclusive scopes, resolves interface questions, receives handoffs, and owns final integration. Subsystem workers own only their packets. A separate verifier or reviewer does not mutate the frozen candidate.

Start bounded workers with a fresh, context-minimized session and a self-contained [packet](task-packet.md). Pass no parent conversation by default.

Prefer a shallow topology: coordinator to subsystem workers, followed by explicit integration and verification. A worker may further delegate only a truly independent subproblem while preserving the same scope, authority, and handoff obligations.

Track worker outcomes as `running`, `evidence_returned`, or `scheduling_failed`, and rebuild the wait set for workers this coordinator launched from its live agent registry before each wait. That registry is not a cross-harness ownership inventory; reconcile repository-wide durable claims through [work-claims](work-claims.md) before dispatch. An ended worker with no evidence becomes `scheduling_failed` and leaves the wait set; never resume it. Reconstruct the same authorized task from the frozen revision and observable invariant in a fresh context. The parent is blocked only when the same concrete impasse recurs for at least three consecutive parent-goal turns after fresh assignment, safe local execution, and every other ready dependency are exhausted.

Do not delegate duplicate investigations or overlapping file or semantic scopes. While workers run, the coordinator advances independent integration, verification, or ready work rather than narrating or polling.

## Workspace isolation

Parallel production edits use separate worktrees or equivalently isolated workspaces based on the same exact revision. Never let multiple workers edit one shared checkout concurrently. Before assignment, inspect active workers, worktrees, branches, claimed file and semantic scopes, and the clone's shared [local workspace state](local-workspace-state.md) for overlap.

Record each worker's bounded scope, base revision, and non-secret workspace identity on the item as a [claim](work-claims.md). Branch names and worker messages alone are not durable ownership.

Workers do not use shared stash state, reset another workspace, rewrite shared history, or clean up another worker's files. Shared databases and listen ports need per-worker names. How a language's build cache, package store, or test runner behaves across worktrees is a consuming-repo problem — do not encode language-specific slot wrappers in this kernel.

## Interfaces and dependencies

Freeze the smallest shared contract before parallel implementation. Record version, owner, consumers, and compatibility expectations. If the contract changes materially, notify dependent workers and rebase their task packets; do not let incompatible assumptions drift until integration.

Represent dependencies explicitly as tracker `blocks` relationships or an equivalent local dependency list. Parallel readiness means dependencies are satisfied, not merely that a worker is available.

## Replenishing pool over fixed waves

A poset of tickets produces dependency-ordered waves. Treat that as a pool, not a batch plan:

- Dispatch up to a worker ceiling **sized for this machine and this repo**: how many compiles and test suites can run at once without thrashing RAM or the disk. A docs-only change can run more workers than a full language build. The user-stated N wins. A consuming `AGENTS.md` may name a default for this repo (and any mutex that serializes a browser or a store). There is no kernel-wide magic number.
- When one worker's handoff is integrated, immediately dispatch the next highest eligible item from a refreshed frontier. Never wait for a whole wave to finish, and never treat two items that merely appear in the same wave as related.
- Refresh after every integration or status change: landing one item can make dependents eligible and can retire items a human moved or canceled.
- File and semantic overlaps impose a scheduling mutex even when both items are dependency-eligible.
- Do not let pool position override a human decision about safety, value, ownership, or a newly discovered dependency. The frontier is an ordering, not an authority engine.

## Integration

Workers return exact commits or patches and evidence; they do not declare the parent outcome complete. Their commits are transport artifacts, not commits to merge individually. The integration owner:

1. confirms every handoff still matches its packet and accepted interface;
2. applies handoffs in dependency order into a clean workspace without merge commits;
3. resolves conflicts with the relevant subsystem owner rather than guessing;
4. runs subsystem tests plus boundary and user-loop tests on the combined result;
5. squashes each accepted feature or fix into its own commit, applies them linearly, and hands the series tip to `$review`, which is the only skill that lands;
6. records remaining work and cleans up workspaces as in [work-claims](work-claims.md).

If integration repeatedly exposes cross-worker coupling, recombine that scope under one owner or redesign the interface. Do not compensate with more coordination ceremony.
