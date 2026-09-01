# Finch review protocol

Use this whenever the skill requires independent review: security, authority, persistence, provider protocol, credential, destructive, or concurrency changes.

The method is iterative and multi-perspective: several reviewers each examine the change from one named angle, a separate pass checks each of their findings before it counts, and the whole thing repeats against the fixed code until a pass turns up nothing new. It adapts the plan-critique loop of #22 to code review; nothing below assumes you have read that issue.

A single unverified pass produces confident nonsense. The two properties that make review mean something are **adversarial verification of every finding** and **an explicit convergence rule**.

Throughout, the *exact tip* means the full 40-character SHA of the commit under review, never a branch name. A branch moves; a review names a commit.

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

Each reviewer returns a list of findings. A finding names the file and the function or line it concerns, what goes wrong there, and the perspective that raised it.

## 3. Verify every finding adversarially

No finding is reported on suspicion alone. Each one must carry a concrete failure scenario: specific inputs, state, or interleaving, leading to a specific wrong outcome.

Verification is a separate pass from discovery, run by an agent that did not produce the finding. Asking the finder to justify its own finding reproduces its original reasoning. *Adversarially* means the verifier's job is to make the finding false — to find the guard, the caller that cannot supply that input, the lock already held. What survives the attempt is what gets reported.

Every finding leaves this pass carrying exactly one of two labels:

- **CONFIRMED** — the verifier traced the code path at the exact tip and can state the failure concretely.
- **PLAUSIBLE** — the concern is real but the scenario is unproven, including when the verifier's counter-argument is itself unproven.

Downgrade rather than discard, and record the verifier's counter-argument next to anything downgraded. A PLAUSIBLE finding is a question for the author, not a defect, and must not block a merge on its own. Only CONFIRMED findings drive fixes and decide convergence.

## 4. Iterate to convergence

A **round** is one complete pass of sections 1 through 3 against a single frozen commit. Concretely, a round:

1. **starts** when the coordinator names the exact tip to review and the branch is frozen — no pushes until the round ends;
2. **re-derives the panel** from the diff at that tip. A fix can add code that earns a perspective the previous round skipped, so the panel is recomputed each round and never inherited;
3. **runs each selected perspective** in its own context per section 2, producing one findings list per perspective;
4. **verifies** every finding in the separate pass of section 3, labeling each CONFIRMED or PLAUSIBLE;
5. **ends** once every selected perspective's findings have been verified and collected. Its artifact is the **round record**: the exact tip, the perspectives selected and skipped, and every finding with its label and failure scenario.

Between rounds the implementer fixes what the round confirmed and pushes; the resulting exact tip is the subject of the next round. A review of the previous tip says nothing about the current one.

A finding is **new** in a round when no earlier round record of this review already holds it — same code location, same failure. Restating a known finding, or re-confirming that a fixed one is fixed, is not new.

**Converged** means a round record contains no new CONFIRMED findings. Convergence takes at least two rounds; one round is a smoke test. If the first round produces no CONFIRMED findings, and so no fixes, run the second round at the same tip with freshly instantiated reviewers, so that it is an independent sample rather than a replay.

**Continue only while the review is improving.** After each round from the second onward, compare its record with the previous one. The round must be **strictly better on both counts**: fewer new CONFIRMED findings, and a less severe worst finding. A run that goes 8 findings to 4, with the worst dropping from a security regression to a recovery-path edge case, is converging and has earned another round. A run that goes 8 to 9, or that trades four small findings for one that corrupts data, is not converging and no number of further rounds will fix it — the change itself is wrong, too large, or the implementer keeps reaching for the same wrong shape.

Stop as soon as a round fails that comparison, even if it is only the second. Waiting for a fixed round count lets a pathological run burn two more rounds before anyone looks at it.

**Six rounds is the backstop, not the test.** It exists so a run that improves by a hair each time still terminates. Reaching it means the same thing as failing the comparison.

Each round's fixes are new code and can introduce new defects — in practice they often do, at the exact lines the previous round's fix touched. That is normal once or twice and is why rounds continue at all; it is also why "keep going until it is clean" is not a safe rule on its own.

Escalate instead. Post the round records per section 5 with the verdict DO NOT MERGE, and hand the coordinator, named one by one, every CONFIRMED finding still unresolved with its failure scenario, the round that raised it, and what was attempted for it. Include the per-round counts and worst severity, so the coordinator can see the trend that triggered the stop rather than only the final state. The coordinator decides what happens next — split the change, narrow its scope, or accept a named risk explicitly. Continuing to loop, dropping the findings, or declaring success anyway are each a failure of this protocol.

## 5. Record the outcome where it can be checked

Post one pull request comment per round, carrying that round's record:

- the exact commit reviewed, and the round number as `round N of at most 3`;
- the perspectives selected, and each skipped one with its reason;
- every CONFIRMED finding with its failure scenario, and its resolution once a later round confirms the fix;
- every PLAUSIBLE finding, explicitly left open;
- on the final round only: whether the review converged or hit the round cap, and the verdict — SAFE TO MERGE, MERGE WITH FIXES, or DO NOT MERGE. A review that hit the cap is DO NOT MERGE plus the escalation of section 4.

"Independently reviewed" in a merge comment must point at these comments. Without them the claim is unfalsifiable, which is the failure mode `AGENTS.md` names: configuration or intent is not conformance.

## Constraints on reviewers

- Reviewers run no Cargo command outside `.agents/skills/finch-backlog/scripts/with-cargo-slot`, and generally should not build at all — reading is the work.
- A reviewer that cannot reach the exact tip commit reports that and stops, rather than reviewing a nearby revision.
- Findings about work owned by another active `finch-work-claim:v1` claim are reported to the coordinator, not fixed.
