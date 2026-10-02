---
name: implement
description: Implement one ready ticket or a direct specified request. Use when a packet, accepted issue, or already-specified user request is ready to code. Does not groom the queue, dispatch siblings, or run review. Use when the user runs /implement.
---

# Implement one unit of work

Take a ready packet (or a direct, already-specified request) and produce a candidate that can be reviewed. This is the worker `$backlog-loop` dispatches. It is not the old all-in-one `backlog` skill: it does not rank the queue, claim sibling scopes, or run the review protocol.

Load the tracker from [`.agents/binding`](../../references/load-binding.md) before any tracker verb.

## Say the tier, then run only that tier

Name the tier in the first message about the change.

| Tier | What it is | Process |
|------|-----------|---------|
| **1 — trivial** | prose, comments, a rename with no behaviour change | no contract, no packet. Run the gate stage the change touches. |
| **2 — ordinary** | a bounded code change with a clear approach and a test that fails before it | an already-authored short [contract](../../references/solution-contract.md), gate stages the scope touches, then `$review`. |
| **3 — risky** | authority, credentials, persistence, wire formats, concurrency, release, or data loss | full contract, production-boundary regression, `$review` of the candidate that will land. Include the [shape questions](../../references/engineering-judgment.md) when the durable record changes. |

A tier is blast radius, not diff size. When two look defensible, pick the lower one and say why. If the diff grows into a higher tier, say so and raise it. A **correction inherits the blast radius of what it touches**. Two lines that change a stored number or a derived total are not tier 1 because they arrived labelled "the fix."

## Contract in, candidate out

- DO expect a tier-2/3 backlog-wrapper ticket to arrive with its solution contract already authored by `$backlog-grooming` (or `$coordinate` for a sibling workstream). Execute that contract; do not re-derive it.
- DO write the short plan yourself, inline, for a direct, already-specified request with no dispatcher in front of you (below).
- DO write the contract yourself if you are running at `strong` lane — the restriction below does not apply.
- DO NOT author or rewrite a tier-2/3 contract yourself while running below `strong` lane. If a backlog-wrapper ticket arrives with no contract, or a thin one, stop and escalate — do not fill the gap. Report back to whatever dispatched you (`$backlog-loop` or `$coordinate`); it runs the `strong`-lane authoring step and resumes you via `resume_from` ([task packet](../../references/task-packet.md)).

## Direct request vs backlog wrapper

- A direct, already-specified request skips issue and claim ceremony: the request plus a short plan is the contract.
- For the [backlog wrapper](../../references/issue-readiness.md): confirm readiness, emit a [claim](../../references/work-claims.md) with `.agents/scripts/emit_claim.py` and post it, use `.agents/scripts/issue_worktree.sh`, then implement the already-authored contract. Preserve unrelated work. Hard-won coordination rules live with the claim protocol.

## Implement, prove, compress

- DO implement the accepted outcome only.
- DO add outcome-appropriate proof: fail-before/pass-after at the visible boundary for a defect; equivalence, integration, and dependency-boundary proof for a refactor; reachability plus tests for a deletion; a usable downstream seam for enabling work.
- DO follow [execution efficiency](../../references/execution-efficiency.md): short handoffs, no recap, do not open the review protocol.
- DO isolate with `.agents/scripts/issue_worktree.sh add <n>`.
- DO check the [task packet](../../references/task-packet.md) for your model lane.
- DO return to specification if the outcome, scope, or authority is still unclear.
- DO name an external dependency that blocks progress.
- DO record it, rather than accumulate ceremony, if evidence shows the outcome cannot be delivered under accepted constraints.
- DO compress before `$review`: remove duplication, speculative abstractions, and tests that merely mirror the implementation. Prefer a coherent result over a mechanically small diff.
- DO emit the separately versioned [boundary-load evidence](../../references/boundary-load.md) requested by the packet when the consuming repository has opted in. Derive modified paths from Git. Preserve inspected-path coverage as `not_observed` or `partial` when the harness cannot establish completeness; neither means zero escapes.
- DO NOT [flatten a named seam](../../references/engineering-judgment.md) during compression.
- DO file incidental dead code as its own deletion ticket — it does not widen this change.
- DO NOT mix a required refactor or migration into the feature. Stack: tests that pin behavior, then the refactor/migration, then the feature. Each lands. Staged replacements use create / switch / delete with an owner and removal trigger.
- DO leave boy scout cleanup the feature does not need for a later ticket.

## Hand off

- DO NOT merge because the tests are green — `$review` is the only skill that lands.
- DO report the exact commit, gates, remaining risks, requested lane, model actually selected, requested boundary-load record when applicable, and next action.
- Sibling coordination belongs to `$coordinate`.
