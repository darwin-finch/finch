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
REPAIR_IN_PROGRESS -> READY_TO_MERGE -> COMPLETE`. The only direct primary skips are
`DRAFT -> READY` with an approved contract and `IN_PROGRESS -> READY_TO_MERGE` with a zero
ledger and clean convergence pass. Side-state evidence never manufactures ownership or
discharges an acceptance gate.

## Closed transition and authority matrix

Every edge not listed below is invalid. Roles are authenticated from immutable GitHub and
claim/contract observations, never trusted from the event's self-declared `actor` field.
Invalid transition attempts are diagnostics and never grant authority.
`owner` is the issue/contract owner; `coordinator` is the stable worker and responsible
actor named by the contract; `claim worker` is the active claim's exact worker/actor.
Initial ownership comes from the issue author or their earlier immutable delegation. A
proxy must have earlier, unedited, operation-specific authorization naming the issue,
event, destination, substitute login, and bounded evidence identity.

| From | To | Authorized poster | Typed evidence |
|---|---|---|---|
| none | DRAFT | issue author/coordinator | outcome, author identity, next clarification |
| none | NEEDS_SPECIFICATION | issue author/coordinator | discovery packet and decision owner |
| none | READY | owner/coordinator | approved contract identity; `legacy-bootstrap` packet when retaining pre-cutover work |
| DRAFT | NEEDS_SPECIFICATION | issue author/coordinator | discovery packet |
| DRAFT | READY | owner/coordinator | approved contract identity |
| DRAFT | DECLINED | issue author | decision packet |
| DRAFT | SUPERSEDED | issue author | exhaustive replacement/gate map |
| NEEDS_SPECIFICATION | READY | owner/coordinator | approved contract identity |
| NEEDS_SPECIFICATION | BLOCKED_EXTERNAL | decision owner/coordinator | external packet |
| NEEDS_SPECIFICATION | INFEASIBLE | owner | infeasibility packet and owner disposition |
| NEEDS_SPECIFICATION | DECLINED | owner | decision packet |
| NEEDS_SPECIFICATION | SUPERSEDED | owner | exhaustive replacement/gate map |
| READY | IN_PROGRESS | coordinator/claim worker | approved contract and active claim |
| READY | NEEDS_SPECIFICATION | owner/coordinator | invalidated contract and question |
| READY | BLOCKED_EXTERNAL | owner/coordinator | external packet |
| READY | INFEASIBLE | owner | infeasibility packet and owner disposition |
| READY | DECLINED | owner | decision packet |
| READY | SUPERSEDED | owner | exhaustive replacement/gate map |
| IN_PROGRESS | REPAIR_IN_PROGRESS | coordinator/claim worker | exact tip, review round, ledger, correction owner |
| IN_PROGRESS | READY_TO_MERGE | coordinator | zero ledger, fresh clean review, integration base, tip, gates, artifact |
| REPAIR_IN_PROGRESS | IN_PROGRESS | coordinator/claim worker | approved new epoch/contract and active claim |
| REPAIR_IN_PROGRESS | READY_TO_MERGE | coordinator | zero ledger, fresh clean review, integration base, tip, gates, artifact |
| READY_TO_MERGE | REPAIR_IN_PROGRESS | coordinator | changed tip/base, opened blocker, or merged slice with active successors |
| READY_TO_MERGE | COMPLETE | owner/coordinator | complete outcome packet |
| any active primary | READY | coordinator | prior `return-ready` terminal and recovery evidence |
| any active primary | NEEDS_SPECIFICATION | owner/coordinator | prior `needs-specification` terminal, invalidated contract, recovery identity |
| any active primary | BLOCKED_EXTERNAL | owner/coordinator | prior `blocked-external` terminal and external packet |
| any active primary | INFEASIBLE | owner | prior `infeasible` terminal, infeasibility packet, owner disposition |
| any active primary | DECLINED | owner | prior `declined` terminal and decision packet |
| any active primary | SUPERSEDED | owner | atomic whole-outcome transfer and exhaustive replacement/gate map |
| IN_PROGRESS | IN_PROGRESS | coordinator/new claim worker | atomic handoff activation, successor claim, contract |
| REPAIR_IN_PROGRESS | REPAIR_IN_PROGRESS | coordinator/new claim worker | atomic handoff activation, successor claim, unchanged ledger/corrections |
| BLOCKED_EXTERNAL | NEEDS_SPECIFICATION | decision owner/coordinator | changed question/constraints |
| BLOCKED_EXTERNAL | READY | owner/coordinator | resumption proof and approved contract; claim still required |
| BLOCKED_EXTERNAL | INFEASIBLE | owner | infeasibility packet and disposition |
| BLOCKED_EXTERNAL | DECLINED | owner | decision packet |
| BLOCKED_EXTERNAL | SUPERSEDED | owner | exhaustive replacement/gate map |
| INFEASIBLE | NEEDS_SPECIFICATION | owner | revised constraints/question |
| INFEASIBLE | DECLINED | owner | decision packet |
| INFEASIBLE | SUPERSEDED | owner | exhaustive replacement/gate map |
| DECLINED | NEEDS_SPECIFICATION | owner | resumed intent/question |
| DECLINED | SUPERSEDED | owner | exhaustive replacement/gate map |

“Any active primary” means each of `IN_PROGRESS`, `REPAIR_IN_PROGRESS`, and
`READY_TO_MERGE`; the reducer expands and tests all fifteen edges independently. `COMPLETE`
and `SUPERSEDED` are terminal. `INFEASIBLE` and `DECLINED` retain their side-state identity
if the owner closes the issue; they never masquerade as successful `COMPLETE`.

### Legacy bootstrap

After the workflow cutover event, only an immutable valid claim created before the cutoff
may migrate. Preserve its PR, branch, worktree, tip, base, and commits. Before mutation,
record an inventory, approve a contract covering the retained diff, recompute collisions,
and independently review the exact tip. Then append `none -> READY` with a typed
`legacy-bootstrap` packet and append ordinary `READY -> IN_PROGRESS` using that preserved
claim. If obligations exist, enter `REPAIR_IN_PROGRESS` normally. Malformed, edited,
unverifiable, post-cutover, or uncorroborated claims are ineligible.
The packet binds the cutover SHA/timestamp and immutable claim `createdAt`; the reducer
itself proves claim time is earlier. A text value such as `"false"` cannot assert that
comparison, collision recheck, or exact-tip review.

### Terminal pairing and scoped completion

A claim terminal accounts for one implementation slice; readiness accounts for the whole
outcome. Post the terminal first for `return-ready`, `blocked-external`,
`needs-specification`, `infeasible`, or `declined`, then append the matching readiness
event referencing the exact terminal URL, claim ID, disposition, worker/author authority,
and evidence identity. Until the pair is admitted, mutation is unauthorized and status is
`PAIRING_REQUIRED`; an incomplete or mismatched pair does not invent a readiness state.

For a successful complete outcome: reach `READY_TO_MERGE`, merge, post claim `complete`
with disposition `slice-complete`, close the issue with all acceptance evidence, then post
`COMPLETE`. For a successful narrow slice with successors: reach `READY_TO_MERGE`, merge,
append `READY_TO_MERGE -> REPAIR_IN_PROGRESS` naming merge/tree proof and already-active
successor claims, then terminalize that scoped claim as `slice-complete`; the parent stays
open. Implementation decomposition never means `SUPERSEDED`; that state requires atomic
replacement of the entire outcome and every gate.

## Append-only event

Post a human-readable summary followed by:

```text
<!-- finch-issue-readiness:v1
event-id: <globally unique lowercase UUID>
issue: <issue number>
prior-state: <state or none for the first event>
prior-event-id: <last accepted readiness event ID or none>
new-state: <state>
actor: <tool/person and stable identity>
owner: <contract owner>
evidence-url: <immutable issue or pull-request comment URL>
evidence-kind: <typed packet/event kind required by the matrix>
evidence-digest: <SHA-256 of immutable evidence body>
contract-id: <contract ID or none>
contract-url: <contract URL with numeric comment ID or none>
contract-digest: <SHA-256 or none>
contract-revision: <revision or none>
implementation-base: <full SHA or none>
plan-reviewer: <worker identity or none>
approval-url: <immutable approval URL or none>
claim-id: <active/terminal claim ID or none>
claim-url: <immutable active/terminal claim URL or none>
ledger-id: <ledger identity or none>
round-id: <review round or none>
exact-tip: <full SHA or none>
integration-base: <full SHA or none>
next-action: <single-line action or exact resumption condition>
authority-comment: <immutable owner authorization URL or none>
timestamp: <UTC RFC 3339>
-->
```

Events are append-only and processed by GitHub `createdAt`, numeric comment ID, then block
order. Apply the claim protocol's immutable metadata, digest, retrieval, and
issuer-authority checks. Authenticate and validate an attempted event before admitting its
ID or predecessor. Malformed, unauthorized, unknown, or semantically invalid attempts are
diagnostics and leave the last accepted state intact; a corrected event can reference the
last accepted predecessor. An edited/deleted/digest-changed previously accepted event,
incomplete authoritative retrieval, or two individually valid successors from one accepted
predecessor makes history `INDETERMINATE`. It never falls back to an authorizing state and
cannot authorize mutation, merge, closure, or further reduction. Recovery requires restored
evidence or an owner-authorized bootstrap rooted in a proven nonauthorizing state.

Destination evidence is typed, not satisfied by generic `evidence-url`. READY binds the
contract, digest, revision, base, plan reviewer, and approval. IN_PROGRESS binds the active
claim. REPAIR binds tip, round, ledger, and correction owner. READY_TO_MERGE binds the
integration base, tip, zero transitive ledger, clean round, gates, and artifact. COMPLETE
binds merge/tree identity, claim terminal, closure, all successor gates, artifact/user
proof, cleanup, and frontier evidence. External, infeasible, declined, and superseded
packets contain every field named in the lifecycle table.

The referenced packet itself is an immutable structured record. Its GitHub URL, raw-body
digest, issue, and kind must exactly equal the readiness event. Boolean fields accept only
lowercase `true` or `false`; a nonempty string such as `"false"` is never truthy evidence.

```text
<!-- finch-workflow-evidence:v1
packet-id: <stable ID>
issue: <number>
kind: <destination-specific packet kind>
<typed fields required by the matrix and destination validator>
timestamp: <UTC RFC 3339>
-->
```

The canonical parser normalizes documented hyphenated names once, attaches trusted
GitHub observation metadata, and passes that same typed record to admission and reduction.
Tests and status tooling must not construct privileged synthetic dictionaries that bypass
the raw-block path.

`INFEASIBLE`, `DECLINED`, issue closure, and ownership substitution require an immutable
authority comment from the contract owner when the posting actor differs. Verify that
comment directly. `SUPERSEDED` additionally requires the exhaustive replacement evidence
in the table. A readiness event is status evidence only: `finch-work-claim:v1` remains the
sole production mutation authority, and branch, pull-request, label, or assignee state does
not substitute for it.
