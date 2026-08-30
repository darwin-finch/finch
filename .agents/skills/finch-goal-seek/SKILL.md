---
name: finch-goal-seek
description: Autonomously work Finch's highest-priority unblocked GitHub issues through isolated implementation, regression testing, independent review, merge evidence, and frontier recomputation. Use for Finch backlog, dogfood, release-gate, issue-swarm, or "FIX SHIT" requests; do not use for a single read-only question or an unrelated repository.
---

# Finch Goal Seek

Drive an outcome-level Finch goal until its stated gate is genuinely satisfied. Treat a merged patch as progress, not as the terminal condition.

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

## Build the ready frontier

1. Translate the goal into explicit acceptance gates: functional, regression, review, dogfood/manual, and release gates.
2. Build a dependency graph from issue bodies and comments.
3. Select the highest-priority unblocked issues. Prefer work that closes a dogfood or release gate, enables several dependents, or repairs a reproduced user failure.
4. Create smaller issues only for independently testable work needed by an outcome-level parent. Link parent and dependency edges. Do not manufacture speculative busywork.
5. Assign independent issues in parallel only when their files and semantic ownership do not overlap.

Recompute this frontier after every merge, newly discovered blocker, or changed issue dependency.

## Isolate and delegate work

- Give each independent implementation its own branch and worktree based on synchronized `origin/main`.
- Give every collaborator a complete task packet using [the task-packet template](references/task-packet.md). A new agent has a new context; never rely on shared conversational memory.
- Keep one coordinator responsible for integration, reviews, issue state, CI evidence, and worktree hygiene.
- Avoid two implementers editing the same files. A reviewer may inspect another agent's frozen branch without editing it.
- Use safe parallelism up to the configured thread limit, but stay within the machine's memory budget. On the 16 GB Finch development host, do not run local Cargo/rustc/build/test jobs; use bounded remote CI and lightweight static checks locally.

## Implement narrowly

1. Reproduce a bug before fixing it whenever deterministic reproduction is possible.
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

Require independent exact-tip review before merging security, authority, persistence, provider protocol, credential, destructive, or concurrency changes. Freeze the reviewed commit; if production code changes, repeat review and affected tests.

Do not describe compilation, mocks, or configuration as live provider/model conformance. Keep manual or live acceptance issues open until the exact real-world workflow succeeds.

## Integrate and account

1. Merge only reviewed, tested work into current `main`.
2. Synchronize `main` and verify the merge commit.
3. Update or close the GitHub issue with commit, regression, review, CI, and remaining-gap evidence.
4. Remove clean worktrees after merge or proven supersession. Preserve unique work by committing and pushing it first.
5. Recompute the ready frontier and immediately continue while an unblocked gate remains.

## Stop conditions

Stop successfully only when the requested dogfood/release outcome has passed its explicit automated and manual gates, not merely when one batch merges.

Stop for user direction only when continuing requires new authority, a material product choice, credentials/live action the user has not authorized, or an external state change. A hard or slow issue is not itself a blocker.

At handoff, report merged commits, closed and remaining gates, running work, exact failures, and the next ready frontier.
