---
name: finch-backlog
description: Autonomously work Finch's highest-priority unblocked GitHub issues through isolated implementation, regression testing, independent review, merge evidence, and frontier recomputation. Use for Finch backlog, release-gate, issue-swarm, or "FIX SHIT" requests; do not use for a single read-only question or an unrelated repository.
---

# Finch Backlog Driver

Drive an outcome-level Finch goal until its stated gate is genuinely satisfied. Treat a merged patch as progress, not as the terminal condition.

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

## Build the ready frontier

1. Translate the goal into explicit acceptance gates: functional, regression, review, dogfood/manual, and release gates.
2. Build a dependency graph from issue bodies and comments.
3. Select the highest-priority unblocked issues. Prefer work that closes a dogfood or release gate, enables several dependents, or repairs a reproduced user failure.
4. Create smaller issues only for independently testable work needed by an outcome-level parent. Link parent and dependency edges. Do not manufacture speculative busywork.
5. Assign independent issues in parallel only when their files and semantic ownership do not overlap.

Recompute this frontier after every merge, newly discovered blocker, or changed issue dependency.

## Claim work before mutation

1. Query and parse active `finch-work-claim:v1` events across all open repository issues, including immutable comment metadata and issuer authority, then inspect assignees, open pull requests, branches, worktrees, and running agents before taking an issue. If claim discovery is unavailable, edited, or incomplete, do not mutate; retry or ask the coordinator.
2. If another active claim overlaps the files or semantic scope, coordinate with that worker or choose another ready issue. Do not create a competing implementation merely because the other worker is a different tool or person.
3. After creating the dedicated branch/worktree and before editing production files, post exactly one `finch-work-claim:v1` GitHub issue comment using [the work-claim protocol](references/work-claims.md). This versioned comment is the sole authoritative ownership mechanism; do not substitute an assignee, label, project field, branch, draft PR, or unstructured prose.
4. Require the returned issue-comment URL, then repeat the repository-wide claim query and apply the protocol's deterministic collision rule. If the comment cannot be posted or verified, do not begin implementation; ask a coordinator to establish the claim.
5. Optionally assign the responsible GitHub user for human accountability. Assignment is informational and never establishes or releases ownership.
6. Repeat the repository-wide authoritative-claim check immediately before widening scope and immediately before merging. Publish the exact append-only, issuer-authorized terminal event when ownership ends so stale claims do not strand work.

## Isolate and delegate work

- Give each independent implementation its own branch and worktree based on synchronized `origin/main`.
- Give every collaborator a complete task packet using [the task-packet template](references/task-packet.md). A new agent has a new context; never rely on shared conversational memory.
- Keep one coordinator responsible for integration, reviews, issue state, CI evidence, and worktree hygiene.
- Avoid two implementers editing the same files. A reviewer may inspect another agent's frozen branch without editing it.
- Use safe parallelism up to the configured thread limit, but stay within the machine's memory budget. On the 16 GB Finch development host, run at most one local Cargo command at a time with `CARGO_BUILD_JOBS=2` unless the user declares a different budget. Enforce that limit across agents and worktrees with `.agents/skills/finch-backlog/scripts/with-cargo-slot`; prose coordination and Cargo's per-target locks are not substitutes for the repository-wide slot. Run every local command that can launch Cargo or `rustc` through the wrapper, including `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo run`, and scripts that invoke Cargo. Formatting and read-only source inspection do not need the slot. If the slot times out or the platform has no supported lock utility, fail closed instead of bypassing it. Prefer focused supervised regressions locally; use CI for broad platform and feature matrices. Never make remote CI a substitute for a live workflow that only the local host can exercise.

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
3. Update or close the GitHub issue with commit, regression, review, CI, remaining-gap evidence, and a completion/release event for its work claim.
4. Remove clean worktrees after merge or proven supersession. Preserve unique work by committing and pushing it first.
5. Recompute the ready frontier and immediately continue while an unblocked gate remains.

## Stop conditions

Stop successfully only when the requested dogfood/release outcome has passed its explicit automated and manual gates, not merely when one batch merges.

Stop for user direction only when continuing requires new authority, a material product choice, credentials/live action the user has not authorized, or an external state change. A hard or slow issue is not itself a blocker.

At handoff, report merged commits, closed and remaining gates, running work, exact failures, and the next ready frontier.
