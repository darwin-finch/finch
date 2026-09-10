---
name: finch-backlog
description: Autonomously work Finch's highest-priority unblocked GitHub issues through isolated implementation, regression testing, independent review, merge evidence, and frontier recomputation. Use for Finch backlog, release-gate, issue-swarm, or "FIX SHIT" requests; do not use for a single read-only question or an unrelated repository.
---

# Finch Backlog Driver

Drive an outcome-level Finch goal until its accepted gate is genuinely satisfied. A branch,
review, pull request, or merge is progress; completion is current-main, user-visible, and
accounted. This is an accountable human/agent procedure, not a machine authorization system.

This repository skill is not Finch's `finch agent` command, `src/agent` loop, or
`.finch/tasks.toml` runtime. Do not substitute those product features.

## Establish authority and readiness

1. Read `AGENTS.md`, the controlling issue and dependency comments, and the immutable
   approved [solution contract](references/solution-contract.md).
2. Audit `git status`, `main`, `origin/main`, branches, remotes, worktrees, open issues and
   pull requests, CI, dependencies, running workers, host memory, and test budget.
3. Use the procedural [issue-readiness checklist](references/issue-readiness.md). Readiness,
   candidate correction, and finding attributes are independent. The coordinator records the
   decision; no script, label, reducer, or status report grants `READY`.
4. Preserve unrelated work. Never delete a dirty, unique, or unpushed worktree.

Procedural `READY` requires an immutable solution contract and fresh independent approval
before a production branch, worktree, claim, mutation, or implementation. An obvious isolated
fix may use a compact complete contract and one fresh plan reviewer. Additional perspectives
come only from actual risk. `NEEDS_SPECIFICATION` permits bounded read-only investigation or
an isolated disposable prototype, never production mutation.

## Build the ready frontier

Translate the goal into functional, regression, review, manual/dogfood, and release gates.
Map dependencies and select the highest-priority unblocked ready issue. Create child issues
only for independently testable work, with every inherited gate retaining exactly one current
owner and proof path. Recompute the frontier after every merge or changed blocker.

## Claim work before mutation

1. Query every open issue for all immutable `finch-work-claim:v1` events, including issuer
   authority, pagination, comment metadata, and corroborating PR/branch/worktree/worker state.
2. Fail closed if discovery is incomplete or another active claim overlaps file or semantic
   scope. Age, assignment, labels, branches, pull requests, and readiness records do not own work.
3. Create a dedicated branch/worktree, then post exactly one claim using
   [the work-claim protocol](references/work-claims.md). `finch-work-claim:v1` and its existing
   base-compatible block are the sole mutation-ownership authority.
4. Require the comment URL and repeat the repository-wide collision check before editing.
   Repeat it before scope expansion, handoff, and merge.
5. Scope expansion requires a revised immutable approved contract, a complete collision check,
   and an issuer-authorized whole-claim replacement covering the new scope, followed by another
   collision check before edits. There is no deferred cutover.

## Isolate and delegate work

- Give independent work its own branch/worktree and a complete
  [task packet](references/task-packet.md).
- Keep one coordinator accountable for integration, review, issue state, CI, and cleanup.
- Reviewer/discovery prototypes run only in isolated disposable exact-tip copies. They never
  mutate the implementation worktree, push, read credentials, perform undeclared external
  effects, merge/cherry-pick, or retain production commits.
- A required unavailable reviewer is recorded `UNAVAILABLE` with the reason and gets at most
  one bounded fresh-context fallback. Failure leaves the gate unresolved unless the repository
  owner explicitly accepts the named risk.
- On the 16 GB host, run at most one Cargo command at a time with `CARGO_BUILD_JOBS=2` through
  `.agents/skills/finch-backlog/scripts/with-cargo-slot`. Fail closed if the slot is unavailable.

## Implement narrowly

Implement the reviewed contract, reproduce deterministic bugs, and add a production-boundary
regression that fails on the claim base and passes at the exact tip. Keep coherent commits,
avoid broad cleanup, and push valuable work promptly. A scope-changing discovery stops edits
until the contract and whole claim are replaced as described above.

## Review as corrective work

Run [the IMPCD review protocol](references/review-protocol.md). Reviewers find and help repair:
each confirmed finding carries a stable identity, concrete failure, correction vector,
deterministic proof, affected invariant, and contract-fit assessment. Confidence, severity,
locality, obligation, and lifecycle are independent axes. Count and severity prioritize work;
they never cancel, waive, resolve, reject, or split an obligation.

Confirmed same-contract blockers and required regression debt remain in bounded repair under
the same contract. Only causally separable concerns become owned independent work. If the same
stable blocker survives two competent repair attempts, end that implementation epoch, run one
bounded independent diagnosis/prototype, and change strategy, representation, contract,
assignment, or executable split. Agent failure never proves infeasibility.

Review reaches its finite endpoint only when the transitive same-contract blocker/regression
ledger is zero and exactly one fresh independent clean exact-tip pass finds no new confirmed
same-contract blocker or required regression debt, with a zero post-pass ledger recheck. Record
rounds, findings, and later dispositions as immutable append-only PR comments; never edit away a
finding or axis history. Do not review that unchanged blocker-free tip again. Production changes
require affected tests and exact-tip review again.

## Split or hand off conservatively

Do not claim atomic transfer. Keep the parent claim active while approving exhaustive disjoint
child scopes and gates. The original issuer then publishes an allowed terminal/supersession or
covering whole-claim replacement; recompute all claims; children claim only released disjoint
scope; recompute before edits. A no-owner interval authorizes no mutation and is safer than
overlap. Replacement records never discharge obligations, and the parent outcome remains open
until every inherited gate's current leaf is merged and proven.

## Integrate and account

Merge only a reviewed exact tip whose transitive ledger is zero, fresh clean pass and affected
gates pass, current integration base is recorded, collision check is fresh, and actual
artifact/user-visible proof exists where applicable. Unrelated failures need evidence on an
untouched base. Stop only for an unresolved finding, unavailable gate, material product choice,
or missing authority.

For issue #406 (practical issue readiness and corrective review), the required order is:

1. Merge PR #542 into then-current `main` and synchronize locally.
2. Prove the merged tree is the reviewed policy; record current-main commit, tests, and CI.
3. Publish and verify the issuer-authorized replacement-claim terminal event.
4. Remove only the clean, pushed, fully integrated PR #542 worktree.
5. Close issue #406 with merge, review/test/CI, tree, claim, and cleanup evidence available.
6. Recompute repository-wide claims and the ready frontier after closure.
7. Publish the immutable retrospective/completion comment on closed issue #406, binding all
   preceding evidence and answering the contract's workflow questions.
8. Retrieve it, verify it is unedited, and record its exact body digest; only then is the
   workflow outcome complete.

PR #542 is integrated at step 1 and fully accounted at step 7. Issue #406 completes only at
step 8. Issue #543 remains open and independent and is never consulted for either result.

## Stop conditions

Every claim ends merged and observable, conservatively superseded by already-owned disjoint
replacement work, or blocked by a named external dependency/missing authority. Review,
cancellation, a closed PR, or a preserved branch is not terminal. Completion requires accepted
gates on current main, ticket closure, current artifact/user-visible proof, valid claim terminal,
safe cleanup, and frontier recomputation. A narrow PR may merge while a broader parent stays open
for concretely owned successors; integration and outcome completion remain distinct.

At handoff, report exact commits, closed and remaining gates, running work, failures, and the
next ready frontier. Never claim provider/model conformance from compilation or mocks.
