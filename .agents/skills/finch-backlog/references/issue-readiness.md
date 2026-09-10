# Finch issue-readiness protocol

Issue readiness answers whether an outcome is sufficiently defined and feasible. It is
orthogonal to candidate correction and to a review finding's attributes: readiness is never
a finding label or blocker state, and a review finding never silently changes feasibility.
Append one immutable `finch-issue-readiness:v1` event for each valid transition.

## Lifecycle

| State | Required evidence and owner | Permitted exits and open/closed rule |
|---|---|---|
| `DRAFT` | Desired outcome; proposer or coordinator owns clarification. | `NEEDS_SPECIFICATION` for bounded discovery, or `READY` after approval. Remains open. |
| `NEEDS_SPECIFICATION` | Discovery contract with reproduction evidence, exact missing decision, concrete question, decision owner, nearest minimally specified outcome, and bounded read-only/disposable-prototype plan. | `READY` through an immutable approved solution contract; `BLOCKED_EXTERNAL`; `INFEASIBLE` evidence awaiting owner decision; owner-authorized `DECLINED`; or concrete `SUPERSEDED`. Remains open. No production claim or production implementation branch is permitted. |
| `READY` | Immutable approved solution contract; event names its URL/comment ID, digest, revision, implementation base, plan reviewer, and approval URL. | `IN_PROGRESS` after a valid production claim, or an evidenced side state. Remains open. Only `READY` work may acquire a production claim or branch. |
| `IN_PROGRESS` | Valid active `finch-work-claim:v1` whose bounded scope implements the approved contract. | `REPAIR_IN_PROGRESS`, `READY_TO_MERGE`, or an evidenced side state. Remains open. |
| `REPAIR_IN_PROGRESS` | Frozen exact tip, round record, nonempty same-contract obligation ledger, and assigned correction vectors. | `IN_PROGRESS` after recontract/split, `READY_TO_MERGE` after convergence, or `BLOCKED_EXTERNAL`. It is progress, never rejection; remains open. |
| `READY_TO_MERGE` | Narrowed contract satisfied; fail-before/pass-after regression; all transitive `BLOCKER` and required `REGRESSION_DEBT` leaves independently `RESOLVED`; one fresh clean exact-tip convergence pass; affected gates on the current integration base; and actual artifact or tree-identity proof. | `COMPLETE` after integration, or back to `REPAIR_IN_PROGRESS` if the tip/base changes or a blocker opens. Remains open. |
| `COMPLETE` | Current-main merge; every original gate including transferred successors satisfied; issue closed; valid claim terminal event; rebuilt/deployed artifact where applicable; user-visible proof; proven-safe worktree cleanup; frontier recomputation. | Terminal successful state. The outcome closes only when the event and issue closure agree. |
| `BLOCKED_EXTERNAL` | Evidence, attempted work, external owner, exact resumption condition, and nearest independent work. | Resume to the last justified primary state when the condition holds, or another evidenced side state. Keep open and recheck on every frontier recomputation; age never implies abandonment or infeasibility. |
| `INFEASIBLE` | Desired outcome, accepted constraints, reproduction, attempted approaches and why each fails, contradiction/platform evidence, nearest achievable alternative, and smallest constraint change. | Contract owner accepts the disposition and decides closure, revises constraints back to `NEEDS_SPECIFICATION`, or supersedes it. Agent or implementation failure is insufficient. Keep open until owner disposition. |
| `DECLINED` | Contract-owner decision that a technically possible outcome will not be pursued, with rationale, consequences, and nearest alternative. | Owner may authorize closure or return it to specification. Only the contract owner may authorize this state. |
| `SUPERSEDED` | Existing replacement issue, approved contract, and valid claim ownership for every inherited gate, plus recovery information for valuable commits/evidence. | Closes only after exhaustive ownership transfer. A proposed rewrite is insufficient. |

The primary sequence is `DRAFT -> NEEDS_SPECIFICATION -> READY -> IN_PROGRESS ->
REPAIR_IN_PROGRESS -> READY_TO_MERGE -> COMPLETE`; a primary state may skip an intermediate
state only when the destination's evidence is already complete. Side-state evidence never
manufactures ownership or discharges an acceptance gate.

## Append-only event

Post a human-readable summary followed by:

```text
<!-- finch-issue-readiness:v1
event-id: <globally unique lowercase UUID>
issue: <issue number>
prior-state: <state or none for the first event>
new-state: <state>
actor: <tool/person and stable identity>
owner: <contract owner>
evidence-url: <immutable issue or pull-request comment URL>
contract-id: <contract ID or none>
contract-url: <contract URL with numeric comment ID or none>
contract-digest: <SHA-256 or none>
contract-revision: <revision or none>
implementation-base: <full SHA or none>
ledger-id: <ledger identity or none>
next-action: <single-line action or exact resumption condition>
authority-comment: <immutable owner authorization URL or none>
timestamp: <UTC RFC 3339>
-->
```

Events are append-only and processed by GitHub `createdAt`, then numeric comment ID.
Apply the claim protocol's immutable metadata, digest, retrieval, and issuer-authority
checks. Invalid transition, missing predecessor, edited/deleted event, incomplete
pagination, or contradictory event is a diagnostic and never silently determines state.
The current state is the last valid transition from the last valid state.

`INFEASIBLE`, `DECLINED`, issue closure, and ownership substitution require an immutable
authority comment from the contract owner when the posting actor differs. Verify that
comment directly. `SUPERSEDED` additionally requires the exhaustive replacement evidence
in the table. A readiness event is status evidence only: `finch-work-claim:v1` remains the
sole production mutation authority, and branch, pull-request, label, or assignee state does
not substitute for it.
