# Finch issue-readiness checklist

Issue readiness asks whether an outcome is sufficiently defined and feasible. Candidate
correction asks whether a proposed change meets the contract. Finding attributes describe one
review concern. These are independent concepts: no finding count, severity, candidate result,
or failed attempt changes readiness or feasibility by implication.

This is a procedural checklist recorded by accountable workers. It does not authenticate
GitHub, grant mutation ownership, authorize merge, or derive a state from caller assertions.
`finch-work-claim:v1` remains the sole mutation-ownership authority.

## Lifecycle

The primary user-facing sequence is
`DRAFT -> NEEDS_SPECIFICATION -> READY -> IN_PROGRESS -> REPAIR_IN_PROGRESS -> READY_TO_MERGE -> COMPLETE`.
`COMPLETION_IN_PROGRESS` is a coordinator-only transient accounting substate for the non-atomic
ordered merge/claim/cleanup/closure/evidence sequence. Evidence-backed side states are
`BLOCKED_EXTERNAL`, `INFEASIBLE`, `DECLINED`, and `SUPERSEDED`.

### Actor grammar

Every state and transition record uses exactly one actor expression:

- `ROLE(x)`: role `x` alone is accountable.
- `ANY_OF(x,y,...)`: one explicitly named role acts and the record says which one.
- `ALL_OF(x,y,...)`: every role is a distinct named principal with distinct independently
  supplied evidence identity; the record carries each principal's stable account/session identity,
  and one principal, role alias, empty identity, or reused evidence cannot satisfy two independent
  roles.
- `DECISION_BY(x); RECORDED_BY(y)`: either `x` directly posts an immutable owner-signed decision,
  whose author identity is the named owner and which has no second actor, or `y` mechanically
  records and verifies a linked prior immutable owner-signed decision by `x`. The link includes a
  canonical comment identity, unedited metadata, and exact body digest. A recorder-only assertion,
  edited/wrong-author decision, empty evidence identity, or extra actor is invalid.

The symbols `/`, `+`, commas, and phrases such as “with approval” are not actor operators. Roles do
not grant authority. `proposer` records the desired outcome; `coordinator` collects evidence and
integrates; `contract_owner` alone chooses `DECLINED`, accepts `INFEASIBLE`, approves or rescinds
`SUPERSEDED`, or invalidates the accepted outcome; `fresh_plan_reviewer` independently reviews the
contract; `claim_owner` is the current valid v1 claimant; `implementation_reviewer` independently
reviews a frozen tip; and `external_owner` controls a named unavailable condition but gains no
Finch mutation authority.

### State invariants

A state is valid only while every field in its row remains true. The accountable owner records the
next action but gains no mutation, merge, closure, or disposition authority from the state.
Each state record is structured as `state`, accountable actor expression plus principal identity,
the row's individually named entry/continuing-evidence fields, one permitted next action, the
target-specific exit predicate, and the issue condition. Opaque placeholders such as `evidence`,
`next-action`, and `exit-evidence` do not satisfy a field. Missing, empty, unknown, or contradictory
fields invalidate the record. Each named immutable URL, comment ID, Git tip, and digest uses the
canonical identity forms required by the referenced protocol.

| State | Accountable state owner | Required entry and continuing evidence | Permitted next actions/destinations | Exit condition | Issue condition |
|---|---|---|---|---|---|
| `DRAFT` | `ROLE(proposer)` | Named proposer, desired outcome, initial evidence or report, and next discovery/specification action. An empty or ownerless issue is invalid. | Clarify to `NEEDS_SPECIFICATION`; admit directly to `READY`; owner-decline to `DECLINED`. | One permitted transition has complete actor and evidence records. | Open. |
| `NEEDS_SPECIFICATION` | `ROLE(contract_owner)` | Exact missing decision among behavior, scope, authority, data shape, invariant, boundary, or proof; concrete question; decision owner; bounded investigation; nearest minimally specified outcome; next action. | Approve to `READY`; wait in `BLOCKED_EXTERNAL`; accept evidence as `INFEASIBLE`; choose `DECLINED`. | Missing decision resolves into an approved contract or authorized side-state disposition. | Open. |
| `READY` | `ROLE(contract_owner)` | Complete immutable solution contract plus exact URL, digest, revision, implementation base, independent approval, owner, boundaries, regression/integration/reversion proof, no unresolved specification gap, and next action to publish and verify the exact-scope v1 claim and enter `IN_PROGRESS`. | Claim into `IN_PROGRESS`; return to `NEEDS_SPECIFICATION`; wait in `BLOCKED_EXTERNAL`; owner-dispose to `INFEASIBLE`, `DECLINED`, or `SUPERSEDED`. | A valid transition record is accepted; production mutation remains forbidden until `IN_PROGRESS`. | Open. |
| `IN_PROGRESS` | `ROLE(claim_owner)` | All `READY` evidence remains current; dedicated branch/worktree; valid active v1 claim for exact scope; complete collision checks; edit grant; preserved current tip; next implementation action. | Enter `REPAIR_IN_PROGRESS`; prove `READY_TO_MERGE`; release to `READY`; respecify; wait; or owner-dispose. | Candidate advances through a permitted transition or ownership is terminalized/preserved. | Open. |
| `REPAIR_IN_PROGRESS` | `ROLE(claim_owner)` | Applicable `IN_PROGRESS` invariants; frozen reviewed tip; stable append-only finding ledger; correction vector/proof path for each open same-contract obligation; current strategy epoch; next repair action. | Continue a new repair epoch; prove `READY_TO_MERGE`; release to `READY`; respecify; wait; or owner-dispose. | One listed transition has valid actor and evidence records. | Open. |
| `READY_TO_MERGE` | `ROLE(coordinator)` | Current valid claim; contract satisfied; base-negative/tip-positive regression; every transitive required leaf resolved; exactly one fresh blocker-free exact-tip pass; affected gates; current-main integration; artifact proof; explicit merge/closure authority; next merge or evidence-repair action. All evidence remains current. | Enter `COMPLETION_IN_PROGRESS`; fall back to `REPAIR_IN_PROGRESS`; release to `READY`; respecify to `NEEDS_SPECIFICATION`; wait in `BLOCKED_EXTERNAL`; or owner-dispose to `DECLINED`/`SUPERSEDED`. | One listed transition has valid actor and evidence records. | Open. |
| `COMPLETION_IN_PROGRESS` | `ROLE(coordinator)` | Exact merged current-main identity and successful ordered completion prefix. Entry requires steps 1–2. Cursor is exactly 2 through 7; all earlier steps have identity-bound evidence and next action is the next numbered step. Claim is active at cursor 2 and terminal from 3; worktree is preserved through 3 and removed from 4; issue is open through 4 and closed from 5; scan exists from 6; retrospective exists from 7. Failed attempts remain diagnostics and never advance the cursor. | Advance exactly one step; retain and retry the same step after failure; wait in `BLOCKED_EXTERNAL`; after step 8 enter `COMPLETE`. No implementation, respecification, decline, or supersession occurs inside accounting; a discovered product regression starts a separately owned issue while truthful accounting continues. | Step 8 retrieves the retrospective, proves it unedited, and records SHA-256. | Open at cursors 2–4; closed at cursors 5–7. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | Source state; preferred resume destination and completion cursor if applicable; unavailable condition/evidence; named external owner; attempted work; exact resumption condition; ownership/preservation disposition; nearest independent work; next recheck action. | Resume after full destination revalidation; otherwise use the deterministic fallback below; owner may choose `DECLINED` only before merge. | Condition is satisfied and a valid destination/fallback record is accepted, or owner declines where permitted. | Open, except a completion-origin block preserves its cursor's open/closed condition and history. |
| `INFEASIBLE` | `ROLE(contract_owner)` | Owner-signed acceptance of outcome, constraints, reproduction, attempts and failures, contradiction/platform evidence, nearest alternative, smallest constraint change, and next owner decision. Agent failure is insufficient. | Owner revises to `NEEDS_SPECIFICATION` or chooses `DECLINED`. | Owner records one permitted decision. | Open; evidence alone cannot close it. |
| `DECLINED` | `ROLE(contract_owner)` | Owner-signed feasible-outcome decision, rationale, consequences, nearest alternative, ownership/preservation disposition, and reopen condition. | Remain terminal or owner reopens to `DRAFT`/`NEEDS_SPECIFICATION` with target-specific evidence. | Terminal unless owner supplies a valid reopen record. | May close only with owner decision and preservation evidence. |
| `SUPERSEDED` | `ROLE(contract_owner)` | Owner-signed decision; actual approved replacements; exhaustive inherited-gate map; exactly one current owner/proof path per leaf; preservation and claim disposition; next successor-accounting action. | Remain while successors execute, or owner rescinds to `NEEDS_SPECIFICATION`. | Owner validly rescinds and respecifies; successor completion does not exit this state. | Open until all leaves are proven; may then close while remaining `SUPERSEDED`, never `COMPLETE` by transfer. |
| `COMPLETE` | `ROLE(coordinator)` | Current-main merge; accepted gates; ticket closure; claim terminal; applicable rebuild/deployment; merged-artifact user proof; safe cleanup; frontier recomputation. | None; a regression or changed outcome starts a new issue/contract. | Terminal. | Closed. |

### Transition matrix

Every destination independently satisfies its full state row. No unlisted edge is permitted.

| From | Actor expression | To | Additional transition evidence |
|---|---|---|---|
| `DRAFT` | `ANY_OF(proposer,coordinator)` | `NEEDS_SPECIFICATION` | Missing decision, concrete question, decision owner, bounded next action. |
| `DRAFT` | `ALL_OF(contract_owner,fresh_plan_reviewer)` | `READY` | Owner adopts and reviewer approves exact immutable contract. |
| `DRAFT` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Target-specific owner decision packet. |
| `NEEDS_SPECIFICATION` | `ALL_OF(contract_owner,fresh_plan_reviewer)` | `READY` | Missing decision resolved; exact immutable contract approved. |
| `NEEDS_SPECIFICATION` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Store `resume-to=NEEDS_SPECIFICATION`. |
| `NEEDS_SPECIFICATION` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `INFEASIBLE` | Complete packet and explicit owner acceptance. |
| `NEEDS_SPECIFICATION` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Complete feasible-outcome decline packet. |
| `READY` | `ROLE(coordinator)` | `IN_PROGRESS` | Valid claim plus pre/post collision proof and edit grant. |
| `READY` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Contract invalidation and missing decision; no active mutation claim. |
| `READY` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Store `resume-to=READY`. |
| `READY` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `INFEASIBLE` | Complete evidence and owner acceptance. |
| `READY` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Complete owner decision. |
| `READY` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `SUPERSEDED` | Approved replacements and exhaustive transfer proof. |
| `IN_PROGRESS` | `ROLE(claim_owner)` | `REPAIR_IN_PROGRESS` | Frozen tip and independently confirmed same-contract obligation. |
| `IN_PROGRESS` | `ALL_OF(implementation_reviewer,coordinator)` | `READY_TO_MERGE` | Full destination evidence. |
| `IN_PROGRESS` | `ROLE(coordinator)` | `READY` | Claim terminal, preservation proof, current contract. |
| `IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Contract invalidation, claim terminal, preservation. |
| `IN_PROGRESS` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Store `resume-to=IN_PROGRESS` with preserved current claim, or `resume-to=READY` after terminal/preservation. |
| `IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `INFEASIBLE` | Complete evidence, owner acceptance, ownership disposition. |
| `IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Complete decision and ownership disposition. |
| `IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `SUPERSEDED` | Approved replacements, exhaustive gates, ownership/preservation. |
| `REPAIR_IN_PROGRESS` | `ROLE(claim_owner)` | `REPAIR_IN_PROGRESS` | New frozen tip or strategy epoch; append-only attempt/finding links. |
| `REPAIR_IN_PROGRESS` | `ALL_OF(implementation_reviewer,coordinator)` | `READY_TO_MERGE` | Full destination evidence. |
| `REPAIR_IN_PROGRESS` | `ROLE(coordinator)` | `READY` | Claim terminal/preservation proof; contract current. |
| `REPAIR_IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Contract invalidation, claim terminal, preservation. |
| `REPAIR_IN_PROGRESS` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Preserve claim and store `resume-to=REPAIR_IN_PROGRESS` while current; otherwise terminalize/preserve and store `resume-to=READY`. |
| `REPAIR_IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `INFEASIBLE` | Complete evidence, owner acceptance, ownership disposition. |
| `REPAIR_IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Complete decision and ownership disposition. |
| `REPAIR_IN_PROGRESS` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `SUPERSEDED` | Approved replacements, exhaustive gates, ownership/preservation. |
| `READY_TO_MERGE` | `ROLE(coordinator)` | `COMPLETION_IN_PROGRESS` | Complete steps 1–2 in order; record cursor 2 and exact next step. |
| `READY_TO_MERGE` | `ANY_OF(implementation_reviewer,coordinator)` | `REPAIR_IN_PROGRESS` | Stale/failed evidence or new same-contract obligation; valid active claim. |
| `READY_TO_MERGE` | `ROLE(coordinator)` | `READY` | Claim terminal and preservation proof; contract current. |
| `READY_TO_MERGE` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Contract invalidation, claim terminal, preservation. |
| `READY_TO_MERGE` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Preserve claim and merge-ready invariants except named external condition, storing `resume-to=READY_TO_MERGE`; or terminalize/preserve and store `resume-to=READY`. |
| `READY_TO_MERGE` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Complete decision and ownership disposition. |
| `READY_TO_MERGE` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `SUPERSEDED` | Approved replacements, exhaustive gates, ownership/preservation. |
| `COMPLETION_IN_PROGRESS` | `ROLE(coordinator)` | `COMPLETION_IN_PROGRESS` | At cursors 2–6 advance exactly one step with resulting prefix invariants. |
| `COMPLETION_IN_PROGRESS` | `ROLE(coordinator)` | `COMPLETION_IN_PROGRESS` | Evidence-repair: retain diagnostic, do not advance, retry same next step. |
| `COMPLETION_IN_PROGRESS` | `ROLE(coordinator)` | `BLOCKED_EXTERNAL` | Store exact cursor/condition and preserve issue/claim/worktree facts. |
| `COMPLETION_IN_PROGRESS` | `ROLE(coordinator)` | `COMPLETE` | From cursor 7, step 8 verifies retrospective immutability and SHA-256. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `NEEDS_SPECIFICATION` | `BE-R-NS`: stored destination matches; condition and destination invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `READY` | `BE-R-RDY`: stored destination matches; condition and destination invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `IN_PROGRESS` | `BE-R-IP`: stored destination matches; condition, active claim, and invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `REPAIR_IN_PROGRESS` | `BE-R-RIP`: stored destination matches; condition, active claim, ledger/epoch, and invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `READY_TO_MERGE` | `BE-R-RTM`: stored destination matches; condition and merge-ready invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `COMPLETION_IN_PROGRESS` | `BE-R-CIP`: stored destination/cursor match; condition, prefix, and cursor-dependent invariants freshly proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `READY` | `BE-F-RDY`: implementation/merge claim terminalized; contract current; preservation proven. |
| `BLOCKED_EXTERNAL` | `ROLE(coordinator)` | `REPAIR_IN_PROGRESS` | `BE-F-RIP`: active claim valid but candidate/merge evidence stale; append correction evidence. |
| `BLOCKED_EXTERNAL` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | `BE-F-NS`: owner invalidates contract; missing decision and ownership/preservation recorded. |
| `BLOCKED_EXTERNAL` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Owner decision only when source is pre-merge and not completion accounting. |
| `INFEASIBLE` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Accepted constraint/target changes and revised question/outcome. |
| `INFEASIBLE` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DECLINED` | Owner accepts consequences and declines constrained outcome. |
| `DECLINED` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `DRAFT` | Owner reopens with proposer, outcome, evidence, discovery action. |
| `DECLINED` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Owner reopens with missing decision/question/decision owner. |
| `SUPERSEDED` | `DECISION_BY(contract_owner); RECORDED_BY(coordinator)` | `NEEDS_SPECIFICATION` | Owner rescinds and records revised outcome plus successor preservation. |

For blocked resumption, the stored destination is preferred but never overrides safety. Direct
resume requires fresh revalidation of every destination invariant. The only deterministic
fallbacks are `READY` when the contract is current but ownership absent (`BE-F-RDY`),
`REPAIR_IN_PROGRESS` when ownership is valid but candidate/merge evidence needs correction
(`BE-F-RIP`), and owner-directed `NEEDS_SPECIFICATION` when the contract is invalid (`BE-F-NS`).
Otherwise remain blocked with an updated recheck. A terminalized claim cannot resume directly to
implementation/repair, and stale merge evidence cannot resume directly to merge readiness.
A completion-origin wait resumes only to `COMPLETION_IN_PROGRESS` after the same structured
completion-prefix validator proves cursor 2 through 7, every successful evidence identity, and
the cursor-exact claim/worktree/issue facts. Failed completion attempts append diagnostics but
must preserve those prefix facts byte-for-byte; later restoration does not cure an intervening
mutation. `READY_TO_MERGE` likewise requires an active claim, including after a wait; a terminal
claim deterministically falls back to `READY` rather than resuming merge readiness.

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

For `REPLACED-BY` or `SPLIT-TO`, publish a separately defined expected inventory containing the
exact original gate set and each gate's one expected owner, claim ID, and proof path. Compare it to
the candidate graph for exact set and identity equality, then verify leaves recursively until each
is merged and proven on current main. Missing, extra, duplicate, cross-gate, arbitrary-owner,
arbitrary-claim, overlapping, cyclic, edited, or merely linked successors never discharge an
obligation. If a child cannot be validly claimed, the retained gate stays open.
Each gate has exactly one current child per generation and one current leaf owner/claim/proof path.
Serial replacement history is append-only, but two concurrent current leaves are invalid even if
the expected inventory lists both.

Finding creation and every later disposition are separate immutable PR comments. A disposition
repeats the stable finding ID and full origin-failing tip, separately names the full disposition-
reviewed tip to which proof is bound, and links exactly one immediate predecessor by canonical URL,
matching numeric ID, and 64-hex SHA-256 body digest. One predecessor has at most one direct
successor; later changes extend that chain. Preserve unchanged axes and record old/new values for
changed axes. Never edit an earlier record. A missing, edited, stale-tip, sibling, or identity-
mismatched predecessor keeps the transitive obligation open.

## Conservative claim transition

Keep the parent claim active while planning and approving exhaustive disjoint children. A whole-
claim replacement reserves a UUID, verifies the original issuer's old-claim `supersede` naming it,
scans until the old claim is inactive, permits no mutation in the no-owner interval, verifies an
ordinary full-scope v1 claim using that UUID, then scans again before edits. Child workers likewise
claim only released disjoint scope. Never permit overlap or describe this sequence as atomic; a
terminal reference alone does not activate its replacement.

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
The valid result is exactly: PR #542 integrated and accounted and issue #406 complete. Issue #543
is independent: its lifecycle state is never read and it is never an acceptance gate, successor
obligation, proof path, or completion dependency for PR #542 or issue #406.
