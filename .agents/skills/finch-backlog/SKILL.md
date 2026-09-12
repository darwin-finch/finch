---
name: finch-backlog
description: Work Finch backlog issues from readiness through implementation, review, merge, and observable completion.
---

# Finch backlog workflow

Help engineers ship reliable changes without turning process into a second product. The governing
objective is to deliver the simplest coherent architecture that satisfies the accepted outcome,
with testing and review proportional to actual risk, and integrate it promptly. The accepted
outcome need not be user-visible: deletion, refactoring, and enabling work are valid outcomes.
Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost proportional to the change; otherwise remove it.
Tooling is advisory mechanical lint, never an authority engine.

## Pick the next item by cost and benefit

Order ready work by `(value × certainty × (1 + unblocking)) / cost`, scored 1–5 per axis and
recorded on the item. Cheap, certain, unblocking changes merge first; review attention is the
scarce resource. See [picking the next item](references/queue.md) for the axes, the two guards that
keep the score honest, what makes an item ready, and the five numbers to record per change.

## Say the tier out loud, then run only that tier

Proportionality fails by being thorough. Name the tier in the first message about a change, and run
that tier's process and no more.

| Tier | What it is | Process |
|------|-----------|---------|
| **1 — trivial** | prose, comments, a rename with no behaviour change, a one-line non-behavioural fix | no contract, no packet, no review round. Run the gate stage the change touches, then merge. |
| **2 — ordinary** | a bounded code change with a clear approach and a test that fails before it | a short contract in the issue, one review round on the tip, gate stages the scope touches. |
| **3 — risky** | authority, credentials, persistence, wire or checkpoint formats, process lifecycle, concurrency, release, or anything a user can lose data to | full contract with an independent contract review, a production-boundary regression, an independent review of the candidate that will merge, and the full gate matrix. |

A tier is about blast radius, not diff size: a two-line change to a permission check is tier 3, and
a five-hundred-line docs move is tier 1. When two tiers look defensible, pick the lower one and say
why; the cost of the heavier process is real and is paid in review attention, which is the scarce
resource.

## Two layers

The generic engineering loop is: understand the accepted outcome; design and make a focused
implementation and compression pass; run relevant regression and integration tests; perform
proportional review and repair until clean; squash-merge; then verify the integrated result.

The GitHub backlog wrapper adds issue readiness, a claim, isolated worktree, pull request, terminal
event, and frontier tracking only when selecting or coordinating shared issue work. For a direct,
already-specified user request, the request plus a short plan is the contract; do not invent issue
or claim ceremony. When the backlog wrapper is active, preserve its procedural collision and claim
rules.

## Backlog wrapper: prepare shared issue

This section applies only when the GitHub backlog wrapper is active. Direct already-specified
requests skip issue readiness, claim publication, dedicated-worktree requirements, pull-request
bookkeeping, terminal events, and frontier tracking.

1. Read `AGENTS.md`, the issue, relevant module docs, and current code.
2. Run the [lightweight readiness check](references/issue-readiness.md). Record what is known,
   what is undecided, who can decide it, and the smallest useful next action.
3. Write a [compact solution contract](references/solution-contract.md) before production work.
   Small, obvious changes should have small contracts. Add detail only for actual risk.
4. Check current branches, worktrees, pull requests, and the procedural claim record for overlap.
   Preserve unrelated and unpushed work.
5. Record the four queue scores and one sentence of justification before starting, and re-score
   if actual cost passes double the estimate.

Readiness, a candidate implementation, and a review finding answer different questions. A green
check or severe finding does not redefine the issue. People remain accountable for readiness,
ownership, approval, merge, and closure.

## Implement narrowly

For direct specified requests, follow the generic focused implementation, compression, tests,
review, integration, and verification loop without GitHub ceremony. When the backlog wrapper is
active, use a dedicated branch/worktree and the existing
[`finch-work-claim:v1`](references/work-claims.md) syntax, with procedural conflict checks. In both
cases, implement the requested outcome, add appropriate proof, avoid unrelated cleanup,
and follow the repository's resource-safe test launchers.

If the desired behavior, scope, authority, or boundary is still unclear, return to specification.
If an external dependency blocks progress, name it. If evidence shows the outcome cannot be
delivered under accepted constraints, record that plainly rather than accumulating ceremony.

## Compress before review

Every pull request gets an architecture and compression pass before review and merge. Remove
duplication, speculative abstractions, and tests that merely mirror the implementation where it is
safe. Prefer a coherent result over either code growth or a mechanically small diff. A bounded
larger change is justified when it removes competing representations, avoids a compatibility layer,
or establishes one clear ownership boundary.

Judge net complexity, not raw line count. Never delete wanted functionality, weaken meaningful
regression coverage, or combine unrelated work merely to shrink a diff. Incidental pre-existing dead
code gets a concrete separate deletion ticket and pull request, naming the exact code and evidence
that it is dead; it does not widen the current review. A prerequisite deletion is valid only when
the code is already dead and independently safe to remove before the new system exists.

## Replace architecture completely

Use this sequence when replacing a subsystem or representation:

1. Create the replacement with focused tests and behavioral-equivalence proof.
2. Switch its production integration points and prove the real application path uses it.
3. Delete the predecessor, obsolete adapters, compatibility scaffolding, and old-only tests; prove
   no unintended references or duplicate behavior remain.

The stages may be commits in one coherent change or safe dependent pull requests. Every staged pull
request must build, pass its affected tests, and be safe to merge. An initially unused replacement
is acceptable only with an owned immediate integration successor. Any temporary coexistence records
its owner, successor, removal trigger, and deletion proof; the parent outcome remains incomplete
until deletion. Code newly obviated by the change is deleted in that change or an explicitly
dependent post-integration cleanup, never left as an indefinite dual architecture.

Prefer a hierarchy of cohesive composed subsystems that can be understood and tested independently
where practical. Give each a narrow deliberate facade, private internals, explicit dependency
direction, co-located documentation/tests/context, and integration tests at its boundaries. Do not
force hierarchy onto trivial code or create a generic contracts junk drawer.

## Review to improve the change

Use the [review protocol](references/review-protocol.md). A reviewer is a find-and-help partner:
identify a concrete failure, explain its impact, suggest the simplest coherent repair, and name a
deterministic check. Finding confidence, severity, locality, obligation, and lifecycle are
independent axes.

Repair confirmed same-contract blockers and required regression debt on the same change. Split
only genuinely separable work with its own owner and proof; never use a split to discard an
acceptance obligation. Counts and severity help prioritize but never cancel or resolve findings.

Review the current candidate with risk-derived perspectives. Repair confirmed in-scope blockers,
rerun affected tests, and review again after any material repair. Merge after the first complete
round with zero confirmed in-scope blockers and relevant tests passing; stop then rather than adding
confidence rounds. Speculative or optional items are nonblocking follow-ups. Findings remain bounded
to behavior introduced, changed, relied upon, or made obsolete by the patch.

If the patch relies on a defective out-of-scope prerequisite, create and claim a separate focused
prerequisite change, land it first, then resume the original. Do not absorb unrelated code.
If the same defect repeats, change strategy or narrow the change rather than repeating identical
review.

## Merge and finish

The ordinary trunk path is: focused patch, relevant affected/integration tests, sensible
risk-proportional review, then squash merge. Check current main for conflicts and mergeability, but
do not require ritual rebases or review restarts merely to reproduce a SHA when GitHub can cleanly
squash. If main later exposes a regression, fix it forward with a focused test and review.

Squash each feature or fix into its own commit on `main`. Never combine separate features or fixes
into one squash, and never integrate with a merge commit instead of squashing.

Match proof to the accepted outcome: user-facing changes need user-visible proof; refactors need
equivalence, integration, and dependency-boundary proof; deletion needs reference/reachability
evidence plus affected tests; enabling work needs a usable downstream seam. Completion requires a
current-main merge and the applicable evidence, plus truthful issue/claim/cleanup accounting. A
pull request merge is progress, not automatically the completion of a broader outcome.

At handoff, report the exact commit, tests, remaining risks, ownership, and next action. Do not
claim provider, model, platform, or release conformance without direct evidence.

## Rules that have already cost us something

**No agent attribution trailers.** Never add `Co-Authored-By:` for a model, or a session URL, to a
commit. `CONTRIBUTING.md` is the policy; the commit author is the human who takes responsibility,
and attribution should not imply accountability an agent cannot hold. Say this explicitly when
delegating: subagents imitate git history, and one session's trailers propagated into another
tool's commits before anyone noticed.

**Never stop a process by pattern.** No `pkill -f`, no `killall`: the pattern matches another
session's server, another worktree's daemon, or the user's own editor. Kill a recorded PID or a
named container, or let the supervisor in `scripts/test_brains.sh` reap its own process group.

## Workspace cleanup

The coordinator owns cleanup; workers never remove another worker's workspace. Create workspaces
outside the system temporary directory so a reboot cannot discard uncommitted work.

- Remove a verification, mutant, or probe workspace as soon as its verdict is recorded. Its
  evidence is the recorded result, not the checkout.
- Remove a worker's workspace, and delete its branch, once `main` contains its accepted change.
  Squashed integration hides ancestry, so use the recorded integration evidence or a tree
  comparison against `main` rather than `git branch --merged`.
- Before removing a workspace that has uncommitted changes or a commit no ref reaches, record that
  state under `refs/salvage/`.
- Stop disposable databases and containers with the workspace that created them.
