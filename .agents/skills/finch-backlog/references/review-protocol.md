# Corrective review protocol

Review is corrective work, not a verdict factory. Every reviewer is a find-and-help partner who
tries to make the contracted outcome safer and easier to ship.

## Review the actual risk

Choose perspectives from the diff: correctness, security, persistence, compatibility, lifecycle,
concurrency, accessibility, operations, or test quality. Do not summon extra reviewers or rounds
just to satisfy a number. Every process step must demonstrably reduce defect risk or improve
shipping confidence at a cost proportional to the change; otherwise remove it.

Review an identified current candidate and relevant integration base. Reproduce important failures at the
boundary users or maintainers actually exercise. Tooling is advisory mechanical lint, never an
authority engine; people own findings, repairs, acceptance, and merge decisions.

## Record actionable findings

A useful finding contains:

- a stable short ID;
- the concrete failure and why it matters;
- the smallest credible correction;
- a deterministic regression or inspection;
- the affected invariant; and
- whether it belongs to the current contract.

Track five independent axes:

- confidence: confirmed or plausible;
- severity: critical, high, medium, or low;
- locality: same-contract or independent;
- obligation: blocker, regression debt, or nonblocking; and
- lifecycle: open, resolved, replaced, or split.

Count and severity prioritize attention. Only evidence and an accountable decision can change a
finding's lifecycle. Later corrections should retain the stable ID so a maintainer can follow what
failed, what changed, and what proof now passes. Ordinary links and concise comments are enough;
do not build cryptographic or executable social-authority machinery around review notes.

## Repair or split

Keep confirmed same-contract blockers and required regression debt in the current repair loop.
Make the smallest coherent correction, run the named regression, and review the new tip.

Split only genuinely separable work whose outcome can be implemented and verified independently.
Give it an owner and proof path, and keep any inherited acceptance obligation visible. A replacement
issue or link does not itself discharge the original obligation.

If a repair approach repeatedly fails, diagnose the cause and change the approach. There is no
fixed attempt count or round count that proves infeasibility, and agent failure is not evidence that
the requested outcome cannot be built.

## Converge once

Convergence is intentionally simple:

1. Resolve every concrete in-scope blocker and required regression-debt item.
2. Review the current candidate with perspectives appropriate to the final diff.
3. Repair confirmed in-scope blockers, rerun affected tests, and start another round after any
   material repair.
4. Merge after the first complete round with zero confirmed in-scope blockers and relevant tests
   passing. Stop then; do not add confidence rounds.

A small patch may need only one perspective; higher-risk changes may need several. Speculative or
optional items are nonblocking follow-ups. Findings are bounded to behavior introduced, changed, or
relied upon by the patch. If a required out-of-scope prerequisite is defective, create and claim a
separate prerequisite change, land it first, then resume; do not absorb unrelated code. If the same
defect repeats, change strategy or narrow the change rather than repeating identical review.

If review finds a blocker, repair it and review the resulting candidate. The ordinary trunk flow is
focused patch, relevant affected/integration tests, risk-proportional review, and squash merge.
Check current main for conflicts and mergeability; do not require a ritual rebase or review restart
solely for commit identity when GitHub can cleanly squash. If main later exposes a regression, fix
it forward. Report unavailable required expertise honestly; do not convert absence into approval.
