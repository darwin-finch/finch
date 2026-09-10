# Finch review protocol

Review is iterative multi-perspective correction (IMPCD): the immutable reviewed solution
contract is the target; a frozen implementation tip is an approximation; independently
verified findings estimate residual error; reviewers return the smallest credible
correction vector and deterministic proof; implementers reproduce coherent corrections;
and the next exact-tip review measures the new approximation. Review is not an outcome and
severity is not a rejection switch.

Use this protocol for every solution contract and for exact-tip implementation review.
An obvious isolated fix uses one fresh independent plan reviewer. Derive an expanded panel
for broader or riskier work. See [solution contracts](solution-contract.md) and
[issue readiness](issue-readiness.md).

The **claim base** is the immutable full SHA in the approved contract and work claim. It
anchors historical diff and fail-before evidence. The **integration base** is current
`main` used for the candidate that will merge. Record both; rebasing never rewrites the
claim base. The **exact tip** is always a full commit SHA, never a moving branch name.

## 1. Review the solution before production edits

After a valid claim and before editing production files, verify the contract identity,
approval, and required content. Give each selected plan perspective a fresh reviewer:

- **Correctness:** the behavior resolves the reproduction and gates.
- **Architecture and scope:** ownership/API/file boundaries are coherent and narrow.
- **Lifecycle, authority, and persistence:** cancellation, restart, identity, permission,
  concurrency, or durable transitions cannot invalidate the plan.
- **Testability:** regression fails on the claim base and current-main integration is
  demonstrable at the real boundary.

Every plan finding names a concrete failure or architectural contradiction and a smallest
credible correction. Resolve confirmed design blockers and record immutable `APPROVE`
before edits. A scope-changing discovery stops edits and requires collision recheck,
immutable contract revision and approval, then append-only claim-scope revision.

## 2. Derive implementation perspectives from the diff

Review `git diff <integration-base>...<exact-tip>` and include every matching row. The
claim base is used only for lineage and fail-before proof; using it for the current panel
would import unrelated upstream changes after `main` advances. Recompute
after each repair because the panel is not inherited. Record every selected perspective
and why every skipped perspective does not apply.

| Perspective | Include when the diff contains |
|---|---|
| Correctness | any behavioral change |
| Concurrency and timing | shared state, locks, async tasks, cancellation, retries, process lifecycle |
| Persistence and format | on-disk layout, schema, serialization, migration, retention |
| Authority and permission | capabilities, credentials, tool gating, peer or remote surfaces |
| Resource and lifecycle | descriptors, processes, memory growth, unbounded accumulation |
| Compatibility | public API, protocol, config, or another branch's consumed surface |
| Test quality | always, including fail-before evidence on the claim base |

## 3. Review independently and safely

Each perspective gets its own fresh context and complete [task packet](task-packet.md).
Reviewers do not see one another's findings before discovery completes. They verify the
exact tip exists and keep the implementation branch/worktree frozen and read-only for the
whole round. They must not edit, commit, push, merge, or cherry-pick there.

A reviewer may prototype only at the frozen exact tip in a disposable isolated worktree
and disposable branch. Prototype authority excludes pushes, credentials, undeclared
external effects, merge, cherry-pick, and mutation of the implementation worktree. Delete
or abandon the disposable prototype after retaining only its finding ID, minimal sketch,
affected invariant, deterministic command/result, and contract fit. The implementer must
reproduce the correction on the unfrozen implementation branch; prototype commits are
never integrated directly.

If an assigned independent reviewer is unavailable, make one bounded replacement attempt
with a fresh reviewer holding the same packet and perspective. Never substitute author
self-review or omit a required perspective. If the replacement or separate verifier is
also unavailable, record the attempts, exact missing evidence, resumption condition, and
nearest independent work; transition the outcome to `BLOCKED_EXTERNAL` and keep the issue,
pull request, claim evidence, and ledger open.

## 4. Verify findings and maintain stable identity

Discovery and verification are separate passes. A verifier who did not originate the
finding tries to disprove its concrete input/state/interleaving-to-wrong-outcome scenario.
Every concern receives one stable finding ID when first recorded, including PLAUSIBLE
concerns. Each append-only event for that ID carries five independent axes:

- **confidence:** `CONFIRMED | PLAUSIBLE`;
- **severity:** `CRITICAL | HIGH | MEDIUM | LOW`;
- **locality:** `SAME_CONTRACT | INDEPENDENT`;
- **obligation:** `BLOCKER | REGRESSION_DEBT | NONBLOCKING`;
- **state:** `OPEN | RESOLVED | REPLACED-BY | SPLIT-TO`.

Changing confidence, severity, locality, obligation, or state appends an event under the
same finding ID. It never overwrites history or implicitly changes another axis.
`NONBLOCKING` is an obligation, not a state. A confirmed failure of an approved gate may
not be `NONBLOCKING`. `PLAUSIBLE` records the verifier's counterargument and cannot block
on confidence alone; `CONFIRMED` records the verified scenario and proof.

Severity estimates impact only and orders repair/escalation priority:

- `CRITICAL`: security, privacy, authority, credentials, unrecoverable data loss, or
  equivalent catastrophic failure;
- `HIGH`: material user-visible correctness, durability, compatibility, or lifecycle;
- `MEDIUM`: bounded correctness, recovery, portability, or required-regression failure;
- `LOW`: diagnostics, maintainability, or test clarity without a current wrong outcome.

Severity and finding count never independently block, resolve, terminate, reject, or cancel
an outcome. Confidence says whether evidence verifies the scenario; obligation says whether
the contract must discharge it; lifecycle state records its append-only progress.

## 5. Classify locality, obligation, and successors

Causal lineage and approved acceptance gates take precedence over changed-file or
subsystem location. A finding caused by the claimed implementation or any repair, or
required for a parent gate, is `SAME_CONTRACT`, including a repair-caused cross-subsystem
finding. If it exceeds the current branch scope, revise the immutable contract and claim or
transfer it to a separately claimed child; never relabel it independent to unblock a merge.

`SAME_CONTRACT` describes obligation, not implementation ownership. It may be implemented
by a child through `SPLIT-TO` while remaining in the parent's completion ledger.
`INDEPENDENT` requires evidence that the concern is causally separable and every approved
parent gate stands without it. Record an actionable independent concern as a durable
`NONBLOCKING` follow-up with its own issue/owner. Independent findings do not use
`SPLIT-TO`, because they transfer no parent obligation.

Only an independently verified `RESOLVED` event with current-main proof discharges an
obligation. `REPLACED-BY` names one or more successor finding IDs and transfers all source
obligation/evidence. `SPLIT-TO` names one or more already-created child issue and claim IDs,
maps every inherited gate and proof, and transfers implementation ownership. Both are
non-discharging: the source remains recursively unresolved until every successor leaf is
independently `RESOLVED` on current main. Follow replacement, return, and nested split
chains transitively; reject missing or cyclic references.

Before a split event, create and link disjoint child issues, allocate every original gate
exactly once, preserve or transfer valuable commits, establish collision-free claims and
worktrees for every child, and start the immediate next slice. Only then append `SPLIT-TO`
and any parent claim-scope revision. The collision-free order is: repository-wide claim
check; immutable child contracts/approvals; parent split reservation; nonauthorizing child
reservations and collision recheck; exact gate/proof mapping; parent activation; parent
finding transition; parent scope-revision event. Until activation the parent remains the
only mutation owner. A proposed future split or closed pull request is not executable.

Completion performs an exhaustive partition of every original acceptance gate: each gate must be directly proven at current main or mapped exactly once through a valid successor
chain whose leaves are proven there. Reject omitted, duplicated, unclaimed, cyclic, or
unresolved leaves. Independent follow-ups remain outside this parent partition.

## 6. Run bounded repair epochs

A round starts by freezing and naming the exact tip and ends only after all selected
perspectives report, separate verification completes, and an append-only round record and
ledger events exist. Each finding carries its stable ID, five axes, scenario, owning
concern, first round, correction vector, regression proof, and disposition.

The implementer applies confirmed same-contract `BLOCKER` corrections and required
`REGRESSION_DEBT`; reviewers do not. `NONBLOCKING` work does not silently widen the patch.
Rising counts, unchanged worst severity, a vague concern decomposing into several precise
ones, and defects revealed on repaired lines are evidence to classify and prioritize—not
automatic stop conditions.

When the same stable blocker survives two competent repair attempts, end that repair
epoch and run exactly one bounded independent diagnosis or safe reviewer prototype. Then
publish and approve an immutable strategy revision or perform the executable split above.
If the fallback fails, cannot run, or requires missing authority, report the exact blocker
and resumption condition as genuinely `BLOCKED_EXTERNAL`; keep the outcome and evidence
open. Never cancel, reject, close, or waive the blocker.

Six implementation rounds in one epoch force the same representation change: bounded
diagnosis followed by immutable recontract or executable split. Six is not a merge cap,
`DO NOT MERGE` trigger, cancellation rule, or permission to waive findings.

Review converges when all transitive same-contract `BLOCKER` and required
`REGRESSION_DEBT` leaves are independently resolved on the current integration base, then
one fresh clean exact-tip pass finds no new blocking obligation. This clean pass is the
finite endpoint; do not repeat it indefinitely. If the first implementation pass is clean,
one fresh independent second pass at the same tip supplies this confirmation.

## 7. Record the round and verdict

One append-only pull-request comment per round records exact tip; claim and integration
bases; selected/skipped perspectives; every finding ID and five axes; scenario and
verification; correction vector; successor links; transitive unresolved ledger; and
whether repair, recontract, split, or fallback is underway.

Use the durable records below. GitHub `createdAt`, numeric comment ID, then block order is
the canonical event order. Retain author, edit metadata, raw-body digest, and URL alongside
each record. Validate authentication, structure, enums, cross-record identity, and prior
event before admitting an event. Invalid attempts are diagnostics and do not consume IDs
or create forks; changed/deleted accepted history, incomplete retrieval, or multiple valid
successors makes the affected history `INDETERMINATE` and nonauthorizing.

```text
<!-- finch-review-round:v1
event-id: <lowercase UUID>
round-id: <stable ID>
ledger-id: <stable ID>
issue: <number>
pull-request: <number>
claim-id: <claim ID>
contract-id: <contract ID>
contract-url: <immutable URL>
contract-digest: <SHA-256>
claim-base: <full SHA>
integration-base: <full current-main SHA>
exact-tip: <full SHA>
round-number: <positive integer>
selected-perspectives: <comma-separated set>
skipped-perspectives: <comma-separated set with reasons in prose>
status: <DISCOVERY|VERIFICATION|REPAIR_IN_PROGRESS|CONVERGED>
verdict: <none|SAFE_TO_MERGE|ESCALATE_WITH_EXECUTABLE_REPAIR_OR_SPLIT>
finding-event-ids: <comma-separated IDs or none>
gate-evidence-url: <immutable URL>
timestamp: <UTC RFC 3339>
-->
```

```text
<!-- finch-review-finding:v1
event-id: <lowercase UUID>
finding-id: <stable ID>
ledger-id: <ledger ID>
round-id: <round ID>
prior-event-id: <previous event for this finding or none>
confidence: <CONFIRMED|PLAUSIBLE>
severity: <CRITICAL|HIGH|MEDIUM|LOW>
locality: <SAME_CONTRACT|INDEPENDENT>
obligation: <BLOCKER|REGRESSION_DEBT|NONBLOCKING>
state: <OPEN|RESOLVED|REPLACED-BY|SPLIT-TO>
exact-tip: <full SHA>
scenario-evidence-url: <immutable scenario/proof URL>
affected-gate-ids: <comma-separated IDs or none>
successor-finding-ids: <comma-separated IDs or none>
child-claim-ids: <comma-separated IDs or none>
owner: <accountable worker/person>
timestamp: <UTC RFC 3339>
-->
```

Every finding event repeats all five axes; an update may change one without implicitly
changing any other. A newly discovered finding uses `prior-event-id: none`. Later events
must name the single accepted predecessor. Round/finding issue, contract, ledger, tip, and
successor identities must agree. Only `CONFIRMED + SAME_CONTRACT + (BLOCKER or required
REGRESSION_DEBT) + non-RESOLVED` enters the blocking ledger. A PLAUSIBLE BLOCKER therefore
retains identity and evidence but cannot prevent merge unless later confirmed.

There are exactly two final review verdicts:

- **SAFE TO MERGE** — convergence and every merge gate are satisfied at the exact tip.
- **ESCALATE WITH EXECUTABLE REPAIR/SPLIT** — a concrete correction, recontract, or already
  executable split continues, or the exact external blocker/resumption condition is open.

`REPAIR IN PROGRESS` is a nonfinal status. `MERGE WITH FIXES` and `DO NOT MERGE` are not
final verdicts. Review never terminalizes an issue or claim.

Before `SAFE TO MERGE`, integrate onto current `main`, record the integration-base SHA,
rerun affected gates, independently review that exact tip, and exercise the actual binary,
generated artifact, deployed process, or documentation tree as applicable. After merge,
synchronize current main and prove the merged artifact was rebuilt from the merge commit
or that its tree is equivalent to the reviewed tip; then demonstrate the user-visible
outcome. Only this post-merge current-main identity proof can support issue completion.

## Reviewer constraints

- Run Cargo only through `scripts/with-cargo-slot`; reading normally needs no build.
- Stop if the exact tip is unavailable instead of reviewing a nearby revision.
- Report work owned by another active claim; do not fix it.
- Never access credentials or perform undeclared external effects.
