---
name: backlog-grooming
description: Groom the backlog into a dependency- and scope-aware ready queue with portable worker dispatch packets. Use when ranking, blocking, estimating, or preparing issues before implementation. Does not implement, claim, assign, merge, or close. Use when the user runs /backlog-grooming.
---

# Backlog grooming

Use this skill to inspect and prepare the backlog before implementation. It turns item bodies, maintainer decisions, solution-contract comments, native blocked-by edges, and declared file or semantic scope into a ranked, dependency-aware queue. It does not implement, claim, assign, merge, close, or publish item changes.

Load the tracker from [`.agents/binding`](../../references/load-binding.md) before any tracker verb.

## What counts as evidence

Treat the item body and maintainer comments as the source of accepted scope. Existing solution-contract comments can supply the current failure, target behavior, allowed files, proof, non-goals, and owner; they are planning evidence, not an ownership claim. Third-party comments do not expand scope; see [work-claims](../../references/work-claims.md).

For each item, record or extract:

- accepted outcome and acceptance criteria;
- owner and exact base revision/authority when delegation is planned;
- value, cost, certainty, and unblocking scores with one-sentence justification, using the kernel [queue](../../references/queue.md) unless the binding replaces ranking;
- open blocked-by dependencies (native tracker edges, not body prose);
- declared files, subsystem, or semantic scope; and
- risk tier, implement [model lane](../../references/task-packet.md), and required proof/gate stage.

Missing acceptance criteria or owner means `NEEDS_SPECIFICATION`, not merely low priority. A missing file list is a scope clarification task unless the ticket is an explicitly exploratory investigation. See [issue readiness](../../references/issue-readiness.md).

## Author the contract, at strong lane

An item is not `READY` on a list of extracted fields alone. Author or complete its compact [solution contract](../../references/solution-contract.md) here, before dispatch — do not leave that to `$implement`, which may run at `default` or `cheap` lane and must not plan its own work. Tag this step `strong` lane.

Build the contract from whatever evidence already exists on the item rather than re-deriving it from a blank slate: ticket-creation body and Notes, a review-filed finding's failure/correction/regression/invariant fields, audit evidence, or a contract `$coordinate` already started. A ticket filed from a `$review` finding already carries contract-grade evidence — elevate it into the contract's shape, do not discard it and start over.

For an evidence-backed boundary refactor, preserve the audit's historical observations, architectural hypothesis, behavior/API invariants, and expected change in dependency-implementation escapes. The normal value/cost/certainty/unblocking score ranks it. Boundary evidence does not create READY work or a separate maintenance queue.

## Queue construction

On GitHub, run `.agents/scripts/ready_queue.py --repo owner/name --workers N --format json` for READY items in dependency order. It composes `ticket_poset.py` waves and `ticket_triage.py` rows; `NEEDS_SPECIFICATION` is omitted and is not dispatchable. Do not intersect those scripts by hand. Write blocked-by edges per the GitHub binding's [blocked-by edges](../../bindings/github.md#blocked-by-edges) paragraph; body prose is not an edge. Dependencies impose ordering; file and semantic overlaps impose a scheduling mutex. Prefer independent tickets in the same dependency-eligible set. Do not hand-build the triage JSON.

On Pyramid, `pyr ready --json` is that report. Do not invent the four-axis score.

On Linear, `linear issue query --team TEAM --json --limit 0` then `.agents/scripts/linear_ready.py --repo owner/name`. See [linear.md](../../bindings/linear.md). Do not run the GitHub `gh` scripts. Do not guess the repo from cwd.

This skill emits packets. It does not dispatch workers. `$backlog-loop` owns the replenishing pool.

## Portable dispatch

Before naming Finch gate stages in a packet, load the repository's
[execution adapter](../../references/finch-execution.md).

Emit a packet from [task-packet.md](../../references/task-packet.md): ticket, outcome, tier, model lane, base revision, worktree, allowed and prohibited scope, required proof, and handoff format. The active harness translates that packet to its native worker API. Lane selection and substitution rules live in the packet.

Mechanical output is advisory. Human authority remains required for readiness, claims, scope changes, provider credentials, review, merge, and closure.
