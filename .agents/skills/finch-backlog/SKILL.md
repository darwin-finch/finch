---
name: finch-backlog
description: Autonomously work Finch's highest-priority unblocked GitHub issues through isolated implementation, regression testing, independent review, merge evidence, and frontier recomputation. Use for Finch backlog, release-gate, issue-swarm, or "FIX SHIT" requests; do not use for a single read-only question or an unrelated repository.
---

# Finch Backlog Driver

Drive an outcome-level Finch goal until its stated gate is genuinely satisfied. The
working reward is a correct patch merged from current `main`, its claimed ticket closed,
and the resulting behavior proved at the user-visible boundary. Reviews, branches, and
pull requests are means to that outcome, never substitute outcomes.

This is a repository-maintenance skill for Codex and Claude Code. It is not Finch's
`finch agent` command, `src/agent` loop, or `.finch/tasks.toml` runtime. Do not fall
back to those product features when this skill is requested or unavailable; report
the missing repository discovery path instead.

## Establish authority

1. Read `AGENTS.md` completely. It is the project invariant source (and currently resolves to `CLAUDE.md`).
2. Read the controlling GitHub issue bodies and dependency comments. GitHub Issues are authoritative; `TODO.md` is an architectural index, not a mutable tracker.
3. Audit before mutation:
   - `git status`, `main`, `origin/main`, branches, remotes, and worktrees;
   - open issues, pull requests, CI, dependency edges, and existing valuable branches;
   - running agents and active work ownership;
   - host memory and the currently declared test/build budget.
4. Preserve unrelated user changes. Never delete a dirty, unique, or unpushed worktree.

If repository state contradicts the issue tracker or user direction, report the evidence and resolve the contradiction before merging.

## Establish issue readiness

Reduce the append-only `finch-issue-readiness:v1` history using
[the issue-readiness protocol](references/issue-readiness.md). Readiness/feasibility,
candidate correction, and finding attributes are three orthogonal layers; never use a
review result or failed implementation as an issue disposition.

Use the pure canonical reducer in `scripts/workflow_protocol.py` for admitted structured
records. GitHub/status tooling gathers complete immutable observations; the reducer owns
transition, authority, pairing, finding, successor, and completion semantics and performs
no discovery or mutation. Validate before admission. Invalid attempts are diagnostics;
tainted accepted history or incomplete retrieval is nonauthorizing `INDETERMINATE`.

Only `READY` work may acquire a production branch or `finch-work-claim:v1` claim. `READY`
requires the immutable, explicitly approved [solution contract](references/solution-contract.md),
including its URL/comment ID, digest, revision, implementation base, plan reviewer, and
approval URL. An obvious isolated fix still uses a complete compact contract and one fresh
independent plan reviewer; derive an expanded panel from actual risk.

When an issue is not ready, transition it to `NEEDS_SPECIFICATION` and perform only bounded
read-only investigation or disposable prototyping under a discovery contract. Record the
missing decision, question, decision owner, nearest minimally specified outcome, and exit
condition. Do not create a production implementation branch or production claim.

## Build the ready frontier

1. Translate the goal into explicit acceptance gates: functional, regression, review, dogfood/manual, and release gates.
2. Build a dependency graph from issue bodies and comments.
3. Select the highest-priority unblocked `READY` issues. Prefer work that closes a dogfood or release gate, enables several dependents, or repairs a reproduced user failure.
4. Create smaller issues only for independently testable work needed by an outcome-level parent. Link parent and dependency edges. Do not manufacture speculative busywork.
5. Assign independent issues in parallel only when their files and semantic ownership do not overlap.

Recompute this frontier after every merge, newly discovered blocker, or changed issue dependency.

## Claim work before mutation

1. Query and parse active `finch-work-claim:v1` events across all open repository issues, including immutable comment metadata and issuer authority, then inspect assignees, open pull requests, branches, worktrees, and running agents before taking an issue. If claim discovery is unavailable, edited, or incomplete, do not mutate; retry or ask the coordinator.
2. If another active claim overlaps the files or semantic scope, coordinate with that worker or choose another ready issue. Do not create a competing implementation merely because the other worker is a different tool or person.
3. After verifying the immutable approved contract and `READY` event, create the dedicated
   production branch/worktree and, before editing production files, post exactly one
   `finch-work-claim:v1` GitHub issue comment using [the work-claim protocol](references/work-claims.md).
   This versioned comment is the sole authoritative ownership mechanism; do not substitute
   a readiness event, assignee, label, project field, branch, draft PR, or prose.
4. Require the returned issue-comment URL, then repeat the repository-wide claim query and apply the protocol's deterministic collision rule. If the comment cannot be posted or verified, do not begin implementation; ask a coordinator to establish the claim.
5. Optionally assign the responsible GitHub user for human accountability. Assignment is informational and never establishes or releases ownership.
6. Repeat the repository-wide authoritative-claim check immediately before widening scope and immediately before merging. Publish the exact append-only, issuer-authorized terminal event when ownership ends so stale claims do not strand work.

## Isolate and delegate work

- Give each independent implementation its own branch and worktree based on synchronized `origin/main`.
- Give every collaborator a complete task packet using [the task-packet template](references/task-packet.md). A new agent has a new context; never rely on shared conversational memory.
- Keep one coordinator responsible for integration, reviews, issue state, CI evidence, and worktree hygiene.
- Avoid two implementers editing the same files. A reviewer may inspect another agent's frozen branch without editing it.
- Use safe parallelism up to the configured thread limit, but stay within the machine's memory budget. On the 16 GB Finch development host, run at most one local Cargo command at a time with `CARGO_BUILD_JOBS=2` unless the user declares a different budget. Enforce that limit across agents and worktrees with `.agents/skills/finch-backlog/scripts/with-cargo-slot`; prose coordination and Cargo's per-target locks are not substitutes for the repository-wide slot. Run every local command that can launch Cargo or `rustc` through the wrapper, including `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo run`, and scripts that invoke Cargo. Formatting and read-only source inspection do not need the slot. If the slot times out or the platform has no supported lock utility, fail closed instead of bypassing it. Prefer focused supervised regressions locally; use CI for broad platform and feature matrices. Never make remote CI a substitute for a live workflow that only the local host can exercise.

## Review the solution contract before implementation

Before a production branch or claim, write and approve the immutable solution contract.
Before editing production code, reverify that exact contract and approval against the
claim. The contract records:

- the reproduced failure and production boundary;
- the observable user outcome and explicit non-goals;
- the invariants that must remain true;
- proposed ownership, API boundaries, and files to change;
- the deterministic regression and why it fails on the claimed base;
- the current-main integration and user-visible proof; and
- how inherited or obsolete work will be reduced, reused, or superseded.

Select plan-review perspectives from correctness, architecture/scope,
lifecycle/authority/persistence, and testability as applicable. Resolve every confirmed
design blocker and record explicit immutable approval before production edits. Obvious
local changes use one fresh plan reviewer and a compact contract; the review is never
skipped. The reviewed contract constrains implementation. Changing the outcome, gates,
files, authority, API boundary, or proof requires edits to stop, another claim check, a new
immutable contract revision and approval, then an append-only claim-scope revision.

## Implement narrowly

1. Implement the reviewed solution contract. Reproduce a bug before fixing it whenever deterministic reproduction is possible.
2. Add a regression that fails on the base revision and passes with the fix. Exercise the production boundary named by `AGENTS.md`, not merely a helper.
3. Keep coherent fixes in separate commits. Avoid repository-wide formatting or unrelated cleanup.
4. Push valuable branches promptly.
5. Never obtain credentials from another application's store. Provider work must use Finch-owned authentication and storage, fail closed across provider/audience boundaries, and avoid external provider-binary dependencies unless an issue explicitly authorizes one.

## Verify and review

For every fix, record the exact commit and exact evidence:

- `git diff --check` and the repository's pinned formatting/static checks;
- the named regression and why it would fail before the fix;
- relevant unit, integration, feature, platform, and release-mode jobs;
- source identity when a temporary CI-only commit/workflow is removed;
- known inherited failures, clearly separated from branch-caused failures.

Run implementation review with [the IMPCD review protocol](references/review-protocol.md).
Derive the panel from the diff and risk, independently verify every finding, preserve its
stable five-axis identity and append-only successor history, and return a concrete
correction vector or executable split for each confirmed obligation. Reviewers may test
only in disposable exact-tip work; they never mutate the frozen implementation worktree,
and the implementer reproduces corrections. Count and severity prioritize work but never
cancel it. One fresh clean exact-tip convergence pass after the transitive ledger reaches
zero is the finite merge endpoint. Freeze the reviewed commit; production changes require
affected tests and exact-tip review again.

Do not describe compilation, mocks, or configuration as live provider/model conformance. Keep manual or live acceptance issues open until the exact real-world workflow succeeds.

## Integrate and account

1. Merge only reviewed, tested work into current `main`.

   **The worker merges; it does not ask.** When the merge conditions below are
   all met, merging is the worker's decision and handing it to the user instead
   is a failure to finish the work. Ask only when a condition cannot be met.

   Merge when every one of these holds:

   - the review protocol reached convergence with no unresolved confirmed
     production blocker at the exact tip being merged, and no declared invariant
     remains without its required regression protection;
   - the named regression and the affected suites pass at that same tip, run
     through the Cargo slot;
   - every repository gate the change touches passes locally or in CI, and any
     gate that could not run is named with the reason;
   - a failing check unrelated to the change is shown to be unrelated by
     evidence, not assumption — reproduce it on an untouched branch or on
     `main` before discounting it;
   - the branch is rebased on current `main`, that distinct integration-base SHA is
     recorded, affected gates pass on that base,
     and the claim check has been repeated immediately beforehand;
   - where the change produces a binary, generated artifact, deployed process,
     or visible interface, the actual result has been exercised and its source
     identity recorded so a stale build cannot masquerade as the merged work.

   Repair unresolved confirmed findings under the reviewed contract. Stop and
   ask only when one cannot be repaired or split within the granted authority,
   a gate cannot be run at all, a material product choice is required, or the
   user has not granted necessary authority. "This change feels significant"
   is not a reason to ask.
2. Synchronize current `main`, verify the merge commit, and prove the resulting artifact
   was rebuilt from that commit or its tree is equivalent to the reviewed tip. Repeat the
   user-visible proof against that identity; pre-merge or stale-artifact evidence cannot
   complete the issue.
3. Close the claimed GitHub ticket when its acceptance gates are met, recording the
   merge commit, regression, review, current-main CI, actual artifact or user-visible
   evidence where applicable, and a completion event for its work claim. When a claimed
   child slice completes only part of a broader parent outcome, close the child ticket
   and update the parent with the remaining gates rather than pretending the parent is
   complete.
   Claim-slice completion and outcome completion are separate. Follow the ordered
   terminal/readiness pairings in `issue-readiness.md`: terminal first for return-to-ready,
   external pause, specification, infeasible, and declined dispositions; READY_TO_MERGE,
   merge, claim terminal, issue close, then COMPLETE for success. A merged narrow slice
   with active successors returns the parent to REPAIR_IN_PROGRESS and leaves it open.
4. Remove clean worktrees after merge or proven supersession. Preserve unique work by committing and pushing it first.
5. Recompute the ready frontier and immediately continue while an unblocked gate remains.

## Stop conditions

Stop successfully only when the requested dogfood/release outcome has passed its explicit automated and manual gates, not merely when one batch merges.

Every claim must end in one of three accountable outcomes: merged and observable;
superseded by already-created, linked, disjoint, claimed replacement slices whose
immediate next slice is continuing; or genuinely blocked by an external dependency or
missing authority. Review, cancellation, a closed pull request, or a preserved branch is
not by itself a terminal outcome. If a solution must split, preserve or transfer valuable
commits and establish the immediate replacement work before closing the superseded pull
request. Do not leave a queue of review-complete but unintegrated work.

Readiness side states follow the evidence and authority rules in `issue-readiness.md`.
`BLOCKED_EXTERNAL` remains open with an exact resumption condition; age is not abandonment.
Agent failure does not prove `INFEASIBLE`; `INFEASIBLE` and `DECLINED` require the contract
owner's disposition. `SUPERSEDED` requires actual replacement contracts and claims for
every inherited gate.

Stop for user direction only when continuing requires new authority, a material product choice, credentials/live action the user has not authorized, or an external state change. A hard or slow issue is not itself a blocker.

At handoff, report merged commits, closed and remaining gates, running work, exact failures, and the next ready frontier.
