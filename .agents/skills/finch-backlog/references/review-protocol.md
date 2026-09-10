# Finch corrective review protocol

Review is iterative multi-perspective correction (IMPCD). The approved immutable solution
contract is the target, the frozen exact-tip patch is the current approximation, verified
findings estimate residual error, correction sketches are correction vectors, and regressions
and integration checks are objective measurements. Reviewers find and help repair; review is
neither punishment nor an outcome by itself.

Use this for solution-contract review and exact-tip implementation review. The **claim base**
is the immutable full SHA in the contract and claim; it anchors lineage and fail-before proof.
The **integration base** is current `main` used for the candidate that will merge. The **exact
tip** is a full commit SHA, never a moving branch.

## Review the solution before production work

Before any production branch, worktree, claim, mutation, or implementation, a fresh reviewer
tries to disprove the contract's behavior, scope, authority, and proof. An obvious isolated fix
uses one reviewer and a compact but complete contract. Expand perspectives only for actual risk.
Every confirmed design blocker receives a concrete failure and smallest credible correction.
Record immutable `APPROVE` only after all blockers are repaired. A changed outcome, gate, file,
authority boundary, or proof requires a new contract revision, approval, collision check, and
whole-claim replacement before edits resume.

## Derive independent perspectives from the diff

Review `git diff <integration-base>...<exact-tip>` and include each applicable perspective:

| Perspective | Include when the diff contains |
|---|---|
| Correctness | any behavioral change |
| Concurrency/timing | shared state, async work, cancellation, retries, process lifecycle |
| Persistence/format | durable data, serialization, migration, retention |
| Authority/permission | capabilities, credentials, tools, claims, remote effects |
| Resource/lifecycle | descriptors, processes, memory, unbounded accumulation |
| Compatibility | public API, protocol, config, or another branch's interface |
| Test quality | always; prove the regression fails on the claim base |
| User-visible integration | artifacts, TUI/CLI/API behavior, deployment, release path |

Record selected perspectives and a concrete reason for every skip. Give each selected
perspective a fresh context and complete task packet. Reviewers inspect a frozen commit and do
not mutate the implementation worktree. Disposable exact-tip copies may be used for bounded
tests or prototypes but may not push, read credentials, perform undeclared external actions,
merge/cherry-pick, or retain production commits.

A required unavailable reviewer is recorded `UNAVAILABLE` with the reason. Try at most one
bounded fresh-context fallback. If it also fails, the gate remains unresolved unless the
repository owner explicitly accepts the named risk.

## Record and verify findings

Each finding has a stable ID that survives restatement and repair, plus:

- concrete input/state/interleaving and wrong outcome;
- smallest credible correction vector and deterministic proof;
- affected invariant and whether the repair fits the approved contract;
- five independent axes:
  `confidence = CONFIRMED | PLAUSIBLE`,
  `severity = CRITICAL | HIGH | MEDIUM | LOW`,
  `locality = SAME_CONTRACT | INDEPENDENT`,
  `obligation = BLOCKER | REGRESSION_DEBT | NONBLOCKING`, and
  `state = OPEN | RESOLVED | REPLACED-BY | SPLIT-TO`.

A separate verifier tries to disprove the finding by tracing guards, callers, authority, and
tests at the exact tip. `CONFIRMED` means the failure survives; `PLAUSIBLE` records the open
question and counterargument. The axes never imply one another. Finding count and severity
prioritize repair but never cancel, close, waive, resolve, reject, or automatically split a
finding. Replacement and split records do not discharge an obligation.

Every confirmed `SAME_CONTRACT` `BLOCKER` and required `REGRESSION_DEBT` stays in bounded
same-contract repair. Only a causally separable concern may become an independent follow-up,
and then it needs an approved contract, disjoint claim, owner, regression, and integration
proof. Each original acceptance gate retains exactly one current owner and proof path through
successor leaves until the leaf is merged and proven on current main. Reviewers check this
accounting explicitly; no tool derives it.

## Repair and converge finitely

A review round freezes one exact tip, re-derives perspectives, independently samples them,
verifies findings, and records the ledger. The implementer reproduces coherent correction
vectors on the implementation worktree. Any production change creates a new tip and requires
affected tests and review again.

Track attempts by stable finding ID. If the same blocker survives two competent repairs, stop
that implementation epoch. Run one bounded independent diagnosis or disposable prototype, then
change strategy, representation, contract, assignment, or executable split. Repeating the same
repair is not progress, and agent failure never proves infeasibility.

The finite convergence rule is exact:

1. resolve every transitive confirmed same-contract blocker and required regression-debt leaf;
2. verify that ledger is zero;
3. run exactly one fresh independent clean pass against the frozen exact tip;
4. converge only if it finds no new confirmed blocker.

Do not run another review of that unchanged blocker-free tip. There is no fixed-round
cancellation and no requirement that count or worst severity monotonically decrease. If the
clean pass finds a blocker, reopen its stable ledger entry, repair it, and repeat from a new tip.

## Frozen-tip corrective disposition

The seven confirmed findings at frozen tip
`372f32e08614b99083072356b2d004a510d9cca9` are repaired, not waived:

- `F406-R3-001`: remove machine admission of contract approvals; retain literal immutable
  contract identity and independent procedural verification.
- `F406-R3-002`: remove caller booleans and prepared-proof inputs that purported to authorize
  readiness or supersession; use the reviewed readiness checklist.
- `F406-R3-003`: remove terminal pairing automation; retain v1 issuer, worker, and immutability
  verification in the existing claim procedure.
- `F406-R3-004`: remove purported atomic handoff; use serialized no-overlap transfer.
- `F406-R3-005`: do not let wrong, edited, duplicate, or merely linked successors discharge
  gates; require exhaustive ownership and proof through current-main leaves.
- `F406-R3-006`: remove evidence-derived convergence; use recorded exact-tip review and tests.
- `F406-R3-007`: remove proxy, legacy-bootstrap, and cutover mechanisms that trusted invented
  identities. Any future authenticated audit service is separate approved product work.

## Verdict and record

Record the claim base, integration base, exact tip, selected/skipped perspectives, reviewer
identities, `UNAVAILABLE` fallbacks, every finding and successor, repair attempts, regressions,
zero-ledger proof, and the single clean pass. `SAFE TO MERGE` is a coordinator/reviewer
procedural conclusion only after these checks and all contract gates pass. It is not ownership,
authentication, or merge authority. The active `finch-work-claim:v1` and explicit task authority
remain controlling.
