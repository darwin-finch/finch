# Corrective review protocol

Review is corrective work, not a verdict factory. Every reviewer is a find-and-help partner who tries to make the contracted outcome safer and easier to ship. [Code review as editing](https://schancel.github.io/2019-07-14-code-reviews-incomplete-guide.html) is the stance: the reviewer is an editor, not a gatekeeper, and not a rubber stamp.

Review the candidate that will land. A finding is a hypothesis until a *different* reviewer walks a concrete failure path.

Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost proportional to the change; otherwise remove it. Tooling is advisory mechanical lint, never an authority engine; people own findings, repairs, acceptance, and merge decisions.

## Review the actual risk

Tier 1 never reaches this skill — [`$review`](../skills/review/SKILL.md) already refuses "lightweight prose edits that carry no behavioral claim" — so every perspective below is being chosen against work that already cleared that bar.

A floor runs on every change that reaches this skill (tier 2 or 3), regardless of what the diff happens to touch:

- **Correctness** — promised observable outcome for valid and invalid inputs.
- **Security** — injection, unsafe deserialization of untrusted input, secret/credential handling, dependency/supply-chain risk, unsafe defaults. Broader than Authority/permission's actor-to-effect mapping below.
- **Efficiency** — algorithmic complexity against the actual data size, N+1 access patterns, unbounded growth, cost proportional to load rather than a fixed constant.
- **Simplicity** — the compression pass below actually ran; the result is the simplest coherent architecture, not merely a small diff.
- **Shape** — [cost of reversal, holes vs frameworks, chunkability, single home](engineering-judgment.md). Weighs heaviest when the diff touches models, persistence, public formats, or module boundaries, but is part of the floor either way.
- **Test quality** — the regression fails at the production boundary on the base; a deliberately broken local variant would not escape it.

Beyond the floor, choose perspectives from what the diff actually does:

- **Concurrency** — duplication, retry, cancellation, timeout, disconnect, replacement, reordered delivery.
- **Persistence/format** — stored data replayed, migrated, bounded, attributed, without loss.
- **Authority/permission** — which actor can cause each effect; data stays in its visibility boundary.
- **Resource lifecycle** — processes, files, memory, queues, artifacts, retained history: bounded and cleaned up.
- **Compatibility** — clients, platforms, schemas, integrations, public contracts.
- **Economic/incentive safety** — minting, double-spend, inflation, misattribution, where value is allocated.
- **Accessibility** — important state and action identifiable without visual coordinates.

Do not summon extra reviewers or rounds beyond this floor just to satisfy a number. A permission check needs Authority/permission and Persistence/format on top of the floor; most ordinary tier-2 changes need only the floor.

**A correction is not a lower bar.** "This is the fix" is a label, not evidence. A wrong correction propagates as fast as the original error. The reviewer verifies against the **original evidence** (the reproduction, the raw inputs, the observed failure on base) — not against the PR description or the commit message. Check that the claimed change is the actual diff. Fail-before on base still applies.

When the repo stores derived figures (totals, daily values, rollups), a mechanical gate that recomputes touched rows from their stated inputs belongs in **that repo's** factory gates. This kernel does not invent a nutrition checker.

Match proof to the outcome: user-facing behavior needs user-visible proof; refactoring needs equivalence, integration, and dependency proof; deletion needs reference/reachability evidence plus tests; enabling work needs a usable downstream seam.

Before review, confirm the architecture and compression pass ran: remove safe duplication, speculative abstractions, and implementation-mirroring tests. Prefer the simplest coherent resulting architecture, not the fewest changed lines. A bounded larger patch can be better when it removes competing representations or compatibility machinery and establishes one clear ownership boundary. Compression must not remove wanted behavior, weaken meaningful regression coverage, mix unrelated work, or [flatten a named seam](engineering-judgment.md).

For a subsystem replacement, verify the complete sequence: the tested equivalent replacement exists; production integration points use it; and the predecessor, obsolete adapters, compatibility paths, and old-only tests are deleted. Dependent pull requests are acceptable when each intermediate state is safe and the temporary coexistence has an owner, immediate successor, removal trigger, and proof. The parent outcome is not complete until deletion.

Where architecture is in scope, check that cohesive subsystems have narrow deliberate facades, private internals, explicit dependency direction, co-located context and tests, and integration proof at their boundaries. Apply this where it improves local reasoning; do not force hierarchy on trivial code.

## Record actionable findings

A useful finding contains:

- a stable short ID;
- the concrete failure and why it matters;
- the simplest coherent correction;
- a deterministic regression or inspection;
- the affected invariant; and
- whether it belongs to the current contract.

A finding is a hypothesis until someone else reproduces it. Confirmed means a **different** reviewer walked a concrete failure path: inputs, state, and the wrong result. A model's own confidence is not that evidence and must not be used as a filter; a second independent pass is. The verifier must actively seek counterevidence in guards, preconditions, callers, and platform contracts.

### Mutation testing: budgeted, not swept

Breaking a guard on purpose to see whether any test notices answers a question nothing else answers, and it costs a full suite run per mutation. Spend it only where a violation would be **silent** — no crash, no failing test, no rejected input, only a wrong state that later validation still accepts.

Budget: at most about five mutations per candidate, chosen from the guards the change itself introduces on an authority, persistence, or history-validation path. Never a blanket sweep of every conditional in the diff.

Cheaper substitutes, in order: the fail-before/pass-after evidence already required for every repair; deleting a single introduced guard and running the suite; coverage over changed lines.

A surviving mutant on a path the change did not touch is a recorded follow-up, not a repair and not a new round. Use disposable fixtures. Never alter the frozen branch.

Track five independent axes:

- confidence: confirmed or plausible;
- severity: critical, high, medium, or low;
- locality: same-contract or independent;
- obligation: blocker, regression debt, or nonblocking; and
- lifecycle: open, resolved, replaced, or split.

Architectural taste without a reversal cost is `PLAUSIBLE` at most. A confirmed shape finding names the future that is already accepted (a ticket, a contract non-goal, a public format) and the migration the current shape would force.

Count and severity prioritize attention. Only evidence and an accountable decision can change a finding's lifecycle.

When a verifier disproves a concern likely to recur, record it as `SETTLED` with the claim, counterevidence, and the premise under which that disposition remains valid. A later round may reopen it only with new evidence or a changed premise.

## Repair, split, or stack

Keep confirmed same-contract blockers and required regression debt in the current repair loop. Make the simplest coherent correction that leaves one clear architectural boundary, run the named regression, and review the new tip.

Split only genuinely separable work whose outcome can be implemented and verified independently. Give it an owner and proof path, and keep any inherited acceptance obligation visible. A replacement issue or link does not itself discharge the original obligation.

Stack (dependent pull requests, or a linear series of per-feature squashes) when the intermediate states are each safe to land and the review of the whole would otherwise be too large to see. Large diffs get the same number of findings as small ones; that is a measured fact, not a vibe. Each stacked unit is reviewed as its own candidate.

If a repair approach repeatedly fails, diagnose the cause and change the approach. There is no fixed attempt count that proves infeasibility, and agent failure is not evidence that the requested outcome cannot be built.

New work discovered in review — a missing hole, a third copy of a pattern, a bug in the same area, campground cleanup — is a ticket, filed through [ticket-creation](../skills/ticket-creation/SKILL.md), not a silent widening of the candidate. If the *feature cannot ship without* a refactor or migration, that is not boy scout: split a stacked predecessor (tests that pin behavior, then the refactor/migration, then the feature). Each lands. The parent stays open until the stack is done.

## Stop on a rule, not on fatigue

A round **ends the change** when each of its confirmed findings is either a product defect now fixed, or a test-only gap recorded as a follow-up. Nothing else keeps the change open.

Classify every confirmed finding as **product** or **test-only** before deciding whether to repair it here. A defect in the code users run is product. A gap in a test, a fixture, or a checker's own regressions is test-only: record it as a follow-up with an owner and merge. **Findings about the tests of the tests do not open a new round.**

Convergence:

1. Resolve every concrete in-scope blocker and required regression-debt item.
2. Review the current candidate with perspectives appropriate to the final diff.
3. Repair confirmed in-scope **product** blockers, rerun affected tests, and review the repaired tip. A repair that touches only tests or fixtures does not start a fresh round.
4. Merge after the first complete round whose confirmed findings are all fixed product defects or recorded test-only follow-ups. Stop then; do not add confidence rounds.

If the same defect repeats, change strategy or narrow the change rather than repeating identical review. If the patch keeps growing, split or redesign it rather than hiding findings.

An integration verdict (`SAFE TO INTEGRATE`, `INTEGRATE AFTER FIXES`, `DO NOT INTEGRATE`) applies only to the exact revision. It does not decide whether the candidate should be repaired, split, reseeded, or abandoned.

## Integration gate

`$review` is the only skill that lands. `$implement` and `$coordinate` produce a candidate. `$backlog-loop` does not merge because a worker reported success.

Land only when:

- the current candidate has a converged review with no unresolved confirmed finding;
- the named regression fails on base and passes on tip, for a fix;
- affected local and CI gates pass at that tip;
- each accepted feature or fix is one squashed commit, and distinct features are not combined;
- the binding's land rule is satisfied ([GitHub squash](../bindings/github.md) carrying the reviewed tip, or [fast-forward to the reviewed tip](../bindings/pyramid.md));
- evidence, terminal ownership events, and cleanup are recorded after integration.

If main later exposes a regression, fix it forward with a focused test and review.

## Delegated review packets

State delegated checks as ordinary, bounded repository correctness work. Name the repository and exact revision, the files or functions to inspect, the local input/state/interleaving, the expected invariant, and the evidence to return. Start the reviewer or verifier in a fresh context without the parent conversation.

A delegation that ends without evidence produces no finding. Mark it `scheduling_failed`, do not resume that worker, and rebuild the same authorized check in a fresh context while other independent work continues.
