# Finch review protocol

Use this whenever the skill requires independent review: first for the solution contract
of nontrivial or risky work, then for the exact implementation tip of security,
authority, persistence, provider protocol, credential, destructive, or concurrency
changes.

The method is iterative and multi-perspective: several reviewers each examine the change from one named angle, a separate pass checks each of their findings before it counts, and the whole thing repeats against the fixed code until a pass turns up nothing new. It adapts the plan-critique loop of #22 to code review; nothing below assumes you have read that issue.

A single unverified pass produces confident nonsense. The properties that make review
mean something are **a reviewed solution boundary**, **adversarial verification of every
finding**, and **an explicit unresolved-blocker model**.

Throughout, the *exact tip* means the full 40-character SHA of the commit under review, never a branch name. A branch moves; a review names a commit.

## 0. Review the solution before code

After the work claim and before production edits, record the solution contract required
by the backlog skill. For an obvious local change this can be compact, but it must still
name the failure boundary, outcome, invariants, proposed files, regression, and
integration proof. Nontrivial, risky, cross-subsystem, or broad work gets independent
fresh reviewers selected from:

- **Correctness:** will the proposed behavior actually resolve the reproduction?
- **Architecture and scope:** does ownership belong here, and is the API/file boundary narrow?
- **Lifecycle, authority, and persistence:** can cancellation, restart, identity, permission,
  or durable-state transitions invalidate the design?
- **Testability:** can the regression fail on the claimed base and can integration be proved
  through the real boundary?

A plan finding must name a concrete failure scenario or architectural contradiction.
Resolve confirmed design blockers in the contract before implementation. Reviewers may
request a narrower contract or already-defined child slices; they do not turn speculative
concerns into implementation requirements. If implementation reveals that the boundary
is wrong, stop editing, revise the contract, repeat the claim check when scope changes,
and review the revised plan before continuing.

## 1. Derive the panel from the diff

The panel is the set of perspectives reviewed in one round, one reviewer per perspective.

Produce the diff before choosing anything:

```sh
git diff <base>...<tip>
```

where `<base>` is the `base` SHA recorded in the change's work claim and `<tip>` is the exact tip. Do not run a fixed checklist. Walk the table once and include every row whose condition the diff actually matches; when a row is arguable, include it. A perspective with nothing to examine is noise that dilutes the findings that matter.

| Perspective | Include when the diff contains |
|---|---|
| Correctness | any behavioral change |
| Concurrency and timing | shared state, locks, async tasks, cancellation, retries, process lifecycle |
| Persistence and format | on-disk layout, schema, serialization, migration, retention |
| Authority and permission | capability checks, credentials, tool gating, peer or remote surfaces |
| Resource and lifecycle | file descriptors, processes, memory growth, unbounded accumulation |
| Compatibility | public API, wire protocol, config surface, anything another agent's branch consumes |
| Test quality | always — including whether the regression genuinely fails on the base revision |

Record the selected perspectives, and each skipped one with a line naming what the diff does not contain, in the round record of section 5. A skipped perspective with a stated reason is evidence; an unexamined one is a gap.

## 2. Review perspectives independently

Give each selected perspective its own agent with its own context and a complete [task packet](task-packet.md). The packet names the single perspective that agent owns, the exact tip, the base SHA and the diff command above, and states that the agent must not edit, commit, or push.

Do not let one perspective see another's findings before reporting, and do not pass along the implementer's framing of what is risky. The implementer's own list of concerns may be supplied as *additional* items to check, never as the scope.

A reviewer inspects a frozen commit: nothing is pushed to the branch between the start of a round and its end. A review of "the branch" is not a review of anything specific.

Each reviewer returns a list of findings. A finding names the file and the function or
line it concerns, what goes wrong there, the perspective that raised it, and the smallest
credible repair that preserves the reviewed solution contract. When the repair cannot
fit that contract, the reviewer names the independently testable child slice and the
current change that can be removed or narrowed. Reviewers remain read-only against the
frozen branch so the reviewed identity stays meaningful, but they may build and test a
candidate patch in disposable work and return that patch or design sketch to the
implementer. Finding defects without helping drive them to resolution is incomplete
review work.

## 3. Verify and classify every finding adversarially

No finding is reported on suspicion alone. Each one must carry a concrete failure scenario: specific inputs, state, or interleaving, leading to a specific wrong outcome.

Verification is a separate pass from discovery, run by an agent that did not produce the finding. Asking the finder to justify its own finding reproduces its original reasoning. *Adversarially* means the verifier's job is to make the finding false — to find the guard, the caller that cannot supply that input, the lock already held. What survives the attempt is what gets reported.

Every finding first leaves this pass carrying exactly one of two verification labels:

- **CONFIRMED** — the verifier traced the code path at the exact tip and can state the failure concretely.
- **PLAUSIBLE** — the concern is real but the scenario is unproven, including when the verifier's counter-argument is itself unproven.

Downgrade rather than discard, and record the verifier's counter-argument next to anything downgraded. A PLAUSIBLE finding is a question for the author, not a defect, and must not block a merge on its own.

Then classify every CONFIRMED finding by effect:

- **Production blocker:** a concrete wrong outcome in the declared solution boundary,
  including security, data, lifecycle, compatibility, or user-visible integration failure.
- **Regression-strengthening debt:** production behavior is correct, but a declared
  invariant lacks a deterministic regression or an existing regression does not exercise
  its promised equivalence class. This blocks only while the invariant remains unprotected
  under `AGENTS.md`.
- **Nonblocking follow-up:** useful hardening, cleanup, or a different concern that is not
  required for the declared outcome and does not invalidate it. Create or link an issue
  when it is actionable; do not expand the current change implicitly.

Record PLAUSIBLE concerns separately as nonblocking questions. Only unresolved confirmed
production blockers and regression debt protecting declared invariants decide final
convergence.

## 4. Iterate to convergence

A **round** is one complete pass of sections 1 through 3 against a single frozen commit. Concretely, a round:

1. **starts** when the coordinator names the exact tip to review and the branch is frozen — no pushes until the round ends;
2. **re-derives the panel** from the diff at that tip. A fix can add code that earns a perspective the previous round skipped, so the panel is recomputed each round and never inherited;
3. **runs each selected perspective** in its own context per section 2, producing one findings list per perspective;
4. **verifies** every finding in the separate pass of section 3, labeling each CONFIRMED or PLAUSIBLE;
5. **ends** once every selected perspective's findings have been verified and collected. Its artifact is the **round record**: the exact tip, the perspectives selected and skipped, and every finding with its label and failure scenario.

Between rounds the implementer fixes confirmed blockers and required regression debt
inside the reviewed contract, then pushes; the resulting exact tip is the subject of the
next round. Nonblocking follow-ups do not silently widen the patch. A review of the
previous tip says nothing about the current one.

A finding is **new** in a round when no earlier round record of this review already holds it — same code location, same failure. Restating a known finding, or re-confirming that a fixed one is fixed, is not new.

**Converged** means the exact-tip blocker ledger is empty: the round confirms every
earlier blocker is resolved and contains no new confirmed production blocker or required
regression debt. Convergence takes at least two rounds; one round is a smoke test. If the
first round produces no blocking findings, and so no fixes, run the second round at the
same tip with freshly instantiated reviewers, so that it is an independent sample rather
than a replay.

Maintain an **unresolved-blocker ledger** across rounds. It names each unresolved
production blocker or regression debt item, its concrete scenario, owning concern, first
round, attempted repair, and current disposition. Continue repairs while that ledger is
shrinking and the reviewed solution boundary remains sound. A same-severity remaining
blocker, a larger raw finding count from a deeper probe, or findings on newly repaired
lines do not by themselves stop useful work.

Use locality and causality to decide repair versus split:

- A finding caused by the original implementation or by lines changed to repair it is
  normal iteration. Keep it in the current repair ledger.
- A finding that introduces a new concern, subsystem, owner, or independently testable
  outcome belongs in a child slice. Narrow the current contract rather than smuggling the
  second project into the fix.
- A finding that proves the selected ownership or API cannot satisfy the outcome
  invalidates the solution contract. Return to section 0 before more code.

Escalate after repeated rounds that do not reduce unresolved blockers, when the
architecture is invalidated, or at six rounds. Six rounds is a backstop for coordinator
intervention, not an automatic cancellation: if a sound repair or executable split is
already progressing, continue that concrete path under updated contracts and claims.

A split is executable only after the coordinator has created and linked child issues with
disjoint acceptance gates, preserved or transferred valuable commits, established valid
claims and worktrees for the replacement work, and continued the immediate next slice.
Do not close the original pull request merely to describe a future split. “Reviewed and
closed” is not a valid terminal outcome.

Mutation review is bounded by the named invariants and relevant equivalence classes in
the solution contract. Every mutant must state which invariant it challenges and why its
class is distinct. Arbitrary mutant counts, blind input enumeration, and demands for
exhaustive sampling are not evidence of better coverage.

Reviewers are adversarial collaborators, not rejection gates. For each confirmed blocker
they recommend a concrete repair, deletion, narrowing, or executable split and identify
the regression that would prove it. The coordinator assigns those repairs and keeps the
ledger moving toward zero. A reviewer may supply a tested candidate diff from disposable
work, but the implementer applies it to the unfrozen branch so authorship, scope, and
exact-tip review remain explicit.

Final convergence still requires at least two independent rounds in the review, a final
round against the exact tip being integrated, and zero unresolved confirmed production
blockers at that tip. Regression-strengthening debt blocks only when it leaves a declared
invariant unprotected under `AGENTS.md`. If the first round has no blocking findings, run
a fresh independent second round at the same tip.

## 5. Record the outcome where it can be checked

Post one pull request comment per round, carrying that round's record:

- the exact commit reviewed and the round number as `round N`;
- the perspectives selected, and each skipped one with its reason;
- every CONFIRMED finding with its failure scenario, classification, owning concern,
  and resolution once a later round confirms the fix;
- every PLAUSIBLE finding, explicitly left open;
- the unresolved-blocker ledger and whether it shrank, plus any repair, contract revision,
  or executable split underway;
- on the final round only: whether the review converged or escalated, and the verdict —
  SAFE TO MERGE, MERGE WITH FIXES, or ESCALATE WITH EXECUTABLE REPAIR/SPLIT. Review alone
  never terminalizes the work.

Before a SAFE TO MERGE verdict, rebase on current `main`, run the affected gates there,
and review the resulting exact tip. Exercise the actual binary, generated artifact, or
user-visible boundary where relevant, and record its source identity. A successful test
against an obsolete base or a stale deployed build is not integration evidence.

The workflow succeeds when the reviewed patch merges from current `main`, the claimed
ticket closes with its evidence, and the observable result matches that merged identity.
A review verdict, including SAFE TO MERGE, earns no terminal credit until integration and
ticket closure actually happen.

"Independently reviewed" in a merge comment must point at these comments. Without them the claim is unfalsifiable, which is the failure mode `AGENTS.md` names: configuration or intent is not conformance.

## Constraints on reviewers

- Reviewers run no Cargo command outside `.agents/skills/finch-backlog/scripts/with-cargo-slot`, and generally should not build at all — reading is the work.
- A reviewer that cannot reach the exact tip commit reports that and stops, rather than reviewing a nearby revision.
- Findings about work owned by another active `finch-work-claim:v1` claim are reported to the coordinator, not fixed.
