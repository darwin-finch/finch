# Finch issue-readiness checklist

Issue readiness asks whether an outcome is sufficiently defined and feasible. Candidate
correction asks whether a proposed change meets the contract. Finding attributes describe one
review concern. These are independent concepts: no finding count, severity, candidate result,
or failed attempt changes readiness or feasibility by implication.

This is a procedural checklist recorded by accountable workers. It does not authenticate
GitHub, grant mutation ownership, authorize merge, or derive a state from caller assertions.
`finch-work-claim:v1` remains the sole mutation-ownership authority.

## Lifecycle

The primary communication sequence is:

`DRAFT -> NEEDS_SPECIFICATION -> READY -> IN_PROGRESS -> REPAIR_IN_PROGRESS -> READY_TO_MERGE -> COMPLETE`

Evidence-backed side states are `BLOCKED_EXTERNAL`, `INFEASIBLE`, `DECLINED`, and
`SUPERSEDED`. Each record names the entry evidence, accountable owner, permitted next action,
exit condition, and whether the issue remains open.

| State | Entry evidence and owner | Exit and next action |
|---|---|---|
| `DRAFT` | Desired outcome; proposer/coordinator owns clarification. | Remain open; specify it or complete the readiness checklist. |
| `NEEDS_SPECIFICATION` | Reproduction, missing decision, concrete question, decision owner, nearest outcome, bounded discovery plan. | Remain open; read-only discovery or disposable prototype only, then contract review. Reject production mutation. |
| `READY` | Coordinator verified the exact immutable complete contract and fresh approval, including URL, digest, revision, claim base, scope, gates, reviewer, and approval URL. | Remain open; acquire and collision-check a valid v1 claim before production work. |
| `IN_PROGRESS` | Valid active v1 claim implementing the approved contract. | Remain open; prove the candidate, enter repair, or record an evidenced side state. |
| `REPAIR_IN_PROGRESS` | Frozen exact tip, verified findings, stable ledger IDs, correction vectors, owners, and attempt counts. | Remain open; repair under the same contract, change strategy after two failed competent attempts, or reach merge readiness. |
| `READY_TO_MERGE` | Zero transitive same-contract blocker/regression ledger, exactly one fresh clean exact-tip pass, regressions and affected gates on current integration base, and artifact/tree proof. | Remain open; merge if task authority permits, or return to repair when tip/base/evidence changes. |
| `COMPLETE` | All controlling ordered completion evidence exists on current main, including gates, claim terminal, closure, visible proof, safe cleanup, frontier, and immutable final proof where required. | Successful terminal outcome; no earlier event alone suffices. |
| `BLOCKED_EXTERNAL` | Attempts, external owner, exact resumption condition, nearest independent work. | Keep open and recheck on each frontier scan; resume when the condition holds. Age is not abandonment. |
| `INFEASIBLE` | Desired outcome/constraints, attempts, contradiction or platform evidence, nearest alternative, smallest constraint change; contract owner decides. | Keep open until owner disposition. Agent failure is insufficient. |
| `DECLINED` | Contract-owner choice not to pursue a feasible outcome, rationale, consequence, alternative. | Owner may close or return it to specification. |
| `SUPERSEDED` | Actual approved replacements and valid disjoint claims owning every inherited gate, plus recovery of valuable work/evidence. | Retain state `SUPERSEDED` and keep the parent open after transfer. Close only after every inherited successor leaf is merged and proven on current main; a proposal, transfer, or record alone is insufficient. |

State records communicate accountable conclusions. They do not independently grant mutation,
external-action, push, merge, close, terminal-event, or cleanup authority.

## Procedural READY checklist

Before recording `READY`, the coordinator and fresh plan reviewer independently inspect:

- the exact unedited contract and approval comments, raw-body digest, IDs, URLs, revision,
  claim base, authors/posters, reviewer identity, and explicit verdict;
- production-boundary failure, observable outcome, non-goals, constraints, hostile cases,
  exact allowed/excluded scope, and reversion;
- every accepted gate with one owner and deterministic proof path;
- fail-before proof on the named base and current-main integration/user-visible proof;
- dependencies, conflicts, inherited work, reviewer availability, and resource limits; and
- whether a compact contract is proportional or actual risk requires more perspectives.

Any mismatch, edit, missing field, incomplete retrieval, unresolved design blocker, or ambiguous
owner leaves the issue in `DRAFT`/`NEEDS_SPECIFICATION`. No script or synthetic fixture may turn
unverified inputs into `READY`.

## Production admission checklist

Even a procedurally ready issue is rejected for production work until all of these separately
hold: explicit edit authority; dedicated production branch/worktree; one valid active
`finch-work-claim:v1`; complete repository-wide immutable claim retrieval; no file or semantic
collision; and a second collision check after the claim. A readiness record is never a claim.

## Repair and successor checklist

For every finding, preserve stable identity and independent confidence, severity, locality,
obligation, and lifecycle axes. Count and severity prioritize attention but cannot cancel,
close, waive, resolve, reject, or split it. Confirmed same-contract blockers and required
regression debt remain repair work; a new exact tip repeats affected tests and review.

For `REPLACED-BY` or `SPLIT-TO`, enumerate every original gate and assign exactly one current
owner and proof path. Verify leaves recursively until each is merged and proven on current main.
A record, successor label, duplicate link, wrong identity, missing claim, overlap, or cycle never
discharges an obligation. If a child cannot be validly claimed, the retained gate stays open.

Finding creation and every later disposition are separate immutable PR comments. A disposition
links the predecessor comment URL/ID and SHA-256 digest, repeats the stable finding ID and exact
tip, preserves unchanged axes, and explicitly records old/new values for changed axes. Never edit
an earlier record to reclassify or resolve it. A missing, edited, or mismatched predecessor keeps
the transitive obligation open.

## Conservative claim transition

Keep the parent claim active while planning and approving exhaustive disjoint children. Then
the original issuer publishes an allowed v1 terminal/supersession or a covering whole-claim
replacement; recompute all open-issue claims; child workers claim released disjoint scope; and
recompute before any edit. A temporary no-owner interval authorizes no mutation. Never permit
overlapping active claims or describe this sequence as atomic.

## Completion checklist

Outcome completion normally requires current-main merge, all accepted gates, ticket closure,
current artifact/user-visible proof, valid issuer-authorized claim terminal, safe cleanup, and
frontier recomputation. PR integration and parent outcome completion are distinct.

For PR #542 and issue #406, verify these eight events in this exact order:

1. PR #542 merged into then-current `main`; local main synchronized.
2. Merged/current-main identity, reviewed policy tree, affected tests, and CI recorded.
3. Replacement #406 claim terminal published and verified issuer-authorized.
4. Only the clean, pushed, fully integrated PR #542 worktree removed.
5. Issue #406 closed with all already-available integration/accounting evidence.
6. Repository-wide claim ledger and ready frontier recomputed after closure.
7. One immutable retrospective/completion comment posted to closed #406, binding steps 1–6,
   exact-tip review and policy paths. It answers which rules produced corrections; which caused
   delay without changing the solution; which were ambiguous or unenforceable; whether any state
   permitted abandonment or false completion; and the smallest evidence-backed follow-up, if any.
8. That comment retrieved, verified unedited, and its exact body digest recorded.

Removing or permuting a prerequisite rejects completion and names the missing/out-of-order event.
The valid final state is exactly: PR #542 integrated and accounted; issue #406 complete; issue
#543 still open and independent. Issue #543 is never an acceptance gate, successor obligation,
proof path, or completion dependency for PR #542 or issue #406.
