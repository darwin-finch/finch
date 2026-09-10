---
name: finch-backlog
description: Work Finch backlog issues from readiness through implementation, review, merge, and observable completion.
---

# Finch backlog workflow

Help engineers ship reliable changes without turning process into a second product.
Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost proportional to the change; otherwise remove it.
Tooling is advisory mechanical lint, never an authority engine.

## Two layers

The generic engineering loop is: understand the requested outcome; make a focused implementation
and compression pass; run relevant regression and integration tests; perform proportional review
and repair until clean; integrate or squash; then verify the shipped result.

The GitHub backlog wrapper adds issue readiness, a claim, isolated worktree, pull request, terminal
event, and frontier tracking only when selecting or coordinating shared issue work. For a direct,
already-specified user request, the request plus a short plan is the contract; do not invent issue
or claim ceremony. When the backlog wrapper is active, preserve its procedural collision and claim
rules.

## Prepare the issue

1. Read `AGENTS.md`, the issue, relevant module docs, and current code.
2. Run the [lightweight readiness check](references/issue-readiness.md). Record what is known,
   what is undecided, who can decide it, and the smallest useful next action.
3. Write a [compact solution contract](references/solution-contract.md) before production work.
   Small, obvious changes should have small contracts. Add detail only for actual risk.
4. Check current branches, worktrees, pull requests, and the procedural claim record for overlap.
   Preserve unrelated and unpushed work.

Readiness, a candidate implementation, and a review finding answer different questions. A green
check or severe finding does not redefine the issue. People remain accountable for readiness,
ownership, approval, merge, and closure.

## Implement narrowly

Use a dedicated branch/worktree and the existing
[`finch-work-claim:v1`](references/work-claims.md) syntax. Keep conflict checks procedural and
human-verifiable. Implement the contracted outcome, add a regression that fails before and passes
after, and avoid unrelated cleanup. Follow the repository's resource-safe test launchers.

If the desired behavior, scope, authority, or boundary is still unclear, return to specification.
If an external dependency blocks progress, name it. If evidence shows the outcome cannot be
delivered under accepted constraints, record that plainly rather than accumulating ceremony.

## Compress before review

Every pull request gets a compression pass before review and merge. Remove duplication,
compatibility scaffolding, speculative abstractions, dead paths, and tests that merely mirror the
implementation where it is safe. Prefer deletion and the smallest behavior-preserving patch.

Judge net complexity, not raw line count. Never delete wanted functionality, weaken meaningful
regression coverage, or combine unrelated work merely to shrink a diff. If compression exposes an
independent prerequisite, make it a separate focused prerequisite pull request, land it first, and
then resume the original change.

## Review to improve the change

Use the [review protocol](references/review-protocol.md). A reviewer is a find-and-help partner:
identify a concrete failure, explain its impact, suggest the smallest credible repair, and name a
deterministic check. Finding confidence, severity, locality, obligation, and lifecycle are
independent axes.

Repair confirmed same-contract blockers and required regression debt on the same change. Split
only genuinely separable work with its own owner and proof; never use a split to discard an
acceptance obligation. Counts and severity help prioritize but never cancel or resolve findings.

Review the current candidate with risk-derived perspectives. Repair confirmed in-scope blockers,
rerun affected tests, and review again after any material repair. Merge after the first complete
round with zero confirmed in-scope blockers and relevant tests passing; stop then rather than adding
confidence rounds. Speculative or optional items are nonblocking follow-ups. Findings remain bounded
to behavior introduced, changed, or relied upon by the patch.

If the patch relies on a defective out-of-scope prerequisite, create and claim a separate focused
prerequisite change, land it first, then resume the original. Do not absorb unrelated code.
If the same defect repeats, change strategy or narrow the change rather than repeating identical
review.

## Merge and finish

The ordinary trunk path is: focused patch, relevant affected/integration tests, sensible
risk-proportional review, then squash merge. Check current main for conflicts and mergeability, but
do not require ritual rebases or review restarts merely to reproduce a SHA when GitHub can cleanly
squash. If main later exposes a regression, fix it forward with a focused test and review.

Confirm any user-visible artifact proof. Completion requires current-main merge and user-visible
evidence where applicable, plus truthful issue/claim/cleanup accounting. A pull request merge is
progress, not automatically the completion of a broader outcome.

At handoff, report the exact commit, tests, remaining risks, ownership, and next action. Do not
claim provider, model, platform, or release conformance without direct evidence.
