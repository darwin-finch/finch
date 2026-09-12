# Corrective review protocol

Review is corrective work, not a verdict factory. Every reviewer is a find-and-help partner who
tries to make the contracted outcome safer and easier to ship.

## Review the actual risk

Choose perspectives from the diff: correctness, security, persistence, compatibility, lifecycle,
concurrency, accessibility, operations, or test quality. Do not summon extra reviewers or rounds
just to satisfy a number. Every process step must demonstrably reduce defect risk or improve
shipping confidence at a cost proportional to the change; otherwise remove it.

Review an identified current candidate and relevant integration base. Match proof to the outcome:
user-facing behavior needs user-visible proof; refactoring needs equivalence, integration, and
dependency proof; deletion needs reference/reachability evidence plus tests; enabling work needs a
usable downstream seam. Tooling is advisory mechanical lint, never an
authority engine; people own findings, repairs, acceptance, and merge decisions.

Before review, confirm that the pull request had an architecture and compression pass: remove safe
duplication, speculative abstractions, and implementation-mirroring tests. Prefer the simplest
coherent resulting architecture, not the fewest changed lines. A bounded larger patch can be better
when it removes competing representations or compatibility machinery and establishes one clear
ownership boundary. Compression must not remove wanted behavior, weaken meaningful regression
coverage, or mix unrelated work.

For a subsystem replacement, verify the complete sequence: the tested equivalent replacement exists;
production integration points use it; and the predecessor, obsolete adapters, compatibility paths,
and old-only tests are deleted. Dependent pull requests are acceptable when each intermediate state
is safe and the temporary coexistence has an owner, immediate successor, removal trigger, and proof.
The parent outcome is not complete until deletion. A prerequisite may delete only code already dead
before the replacement; code made obsolete by this work belongs in the change or a dependent
post-integration cleanup. Incidental pre-existing dead code becomes a concrete separate deletion
ticket and does not widen this review.

Where architecture is in scope, check that cohesive subsystems have narrow deliberate facades,
private internals, explicit dependency direction, co-located context and tests, and integration
proof at their boundaries. Apply this where it improves local reasoning; do not force hierarchy on
trivial code or funnel unrelated APIs into a generic contracts module.

## Record actionable findings

A useful finding contains:

- a stable short ID;
- the concrete failure and why it matters;
- the simplest coherent correction;
- a deterministic regression or inspection;
- the affected invariant; and
- whether it belongs to the current contract.

A finding is a hypothesis until someone else reproduces it. Confirmed means a **different**
reviewer walked a concrete failure path: inputs, state, and the wrong result. A model's own
confidence is not that evidence and must not be used as a filter; a second independent pass is.

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
Make the simplest coherent correction that leaves one clear architectural boundary, run the named
regression, and review the new tip.

Split only genuinely separable work whose outcome can be implemented and verified independently.
Give it an owner and proof path, and keep any inherited acceptance obligation visible. A replacement
issue or link does not itself discharge the original obligation.

If a repair approach repeatedly fails, diagnose the cause and change the approach. There is no
fixed attempt count or round count that proves infeasibility, and agent failure is not evidence that
the requested outcome cannot be built.

## Stop on a rule, not on fatigue

A round **ends the change** when each of its confirmed findings is either a product defect now
fixed, or a test-only gap recorded as a follow-up. Nothing else keeps the change open.

Classify every confirmed finding as **product** or **test-only** before deciding whether to repair
it here. A defect in the code users run is product. A gap in a test, a fixture, or a checker's own
regressions is test-only: record it as a follow-up with an owner and merge. **Findings about the
tests of the tests do not open a new round.** Four rounds on one sibling repository's candidate
produced about thirty confirmed problems; three mattered, and the most expensive finding of the
last round was a race in a test harness.

Convergence is otherwise simple:

1. Resolve every concrete in-scope blocker and required regression-debt item.
2. Review the current candidate with perspectives appropriate to the final diff.
3. Repair confirmed in-scope **product** blockers, rerun affected tests, and review the repaired
   tip. A repair that touches only tests or fixtures does not start a fresh round.
4. Merge after the first complete round whose confirmed findings are all fixed product defects or
   recorded test-only follow-ups. Stop then; do not add confidence rounds.

A small patch may need only one perspective; higher-risk changes may need several. Speculative or
optional items are nonblocking follow-ups. Findings are bounded to behavior introduced, changed,
relied upon, or made obsolete by the patch. If a required out-of-scope prerequisite is defective,
create and claim a separate prerequisite change, land it first, then resume; do not absorb
unrelated code. If the same
defect repeats, change strategy or narrow the change rather than repeating identical review.

If review finds a blocker, repair it and review the resulting candidate. The ordinary trunk flow is
focused patch, relevant affected/integration tests, risk-proportional review, and squash merge.
Check current main for conflicts and mergeability; do not require a ritual rebase or review restart
solely for commit identity when GitHub can cleanly squash. If main later exposes a regression, fix
it forward. Report unavailable required expertise honestly; do not convert absence into approval.
