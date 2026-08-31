# Finch review protocol

Applies the iterative multi-perspective method of #22 (IMCPD) to code review. Use it whenever the skill requires independent review: security, authority, persistence, provider protocol, credential, destructive, or concurrency changes.

A single unverified pass produces confident nonsense. The two properties that make review mean something are **adversarial verification of every finding** and **an explicit convergence rule**.

## 1. Derive the panel from the diff

Do not run a fixed checklist. Read the diff first and select perspectives it actually earns. A perspective with nothing to examine is noise that dilutes the findings that matter.

| Perspective | Include when the diff contains |
|---|---|
| Correctness | any behavioral change |
| Concurrency and timing | shared state, locks, async tasks, cancellation, retries, process lifecycle |
| Persistence and format | on-disk layout, schema, serialization, migration, retention |
| Authority and permission | capability checks, credentials, tool gating, peer or remote surfaces |
| Resource and lifecycle | file descriptors, processes, memory growth, unbounded accumulation |
| Compatibility | public API, wire protocol, config surface, anything another agent's branch consumes |
| Test quality | always — including whether the regression genuinely fails on the base revision |

Record which perspectives were selected and which were deliberately skipped. A skipped perspective with a stated reason is evidence; an unexamined one is a gap.

## 2. Review perspectives independently

Give each perspective its own agent with its own context and a complete task packet. Do not let one perspective see another's findings before reporting, and do not pass along the implementer's framing of what is risky. The implementer's own list of concerns may be supplied as *additional* items to check, never as the scope.

A reviewer inspects a frozen commit and never edits, commits, or pushes. Name the exact tip commit in the packet; a review of "the branch" is not a review of anything specific.

## 3. Verify every finding adversarially

No finding is reported on suspicion alone. Each one must carry a concrete failure scenario: specific inputs, state, or interleaving, leading to a specific wrong outcome.

- **CONFIRMED** — the reviewer traced the code path and can state the failure concretely.
- **PLAUSIBLE** — the concern is real but the scenario is unproven.

Downgrade rather than discard. A PLAUSIBLE finding is a question for the author, not a defect, and must not block a merge on its own.

Verification is a separate pass from discovery, ideally by a different agent. Asking the finder to justify its own finding reproduces its original reasoning.

## 4. Iterate to convergence

After fixes land, re-review at the **new** exact tip. A fix changes the code the earlier review examined, and a review of the previous tip says nothing about the current one.

Stop when a full round produces no new CONFIRMED findings. Record the number of rounds. One round is a smoke test, not a convergence.

## 5. Record the outcome where it can be checked

Post to the pull request:

- the exact commit reviewed, and the perspectives selected and skipped;
- every CONFIRMED finding with its failure scenario and resolution;
- every PLAUSIBLE finding, explicitly left open;
- the number of rounds and the convergence result;
- the verdict: SAFE TO MERGE, MERGE WITH FIXES, or DO NOT MERGE.

"Independently reviewed" in a merge comment must point at this record. Without it the claim is unfalsifiable, which is the failure mode `AGENTS.md` names: configuration or intent is not conformance.

## Constraints on reviewers

- Reviewers run no Cargo command outside `.agents/skills/finch-backlog/scripts/with-cargo-slot`, and generally should not build at all — reading is the work.
- A reviewer that cannot reach the exact tip commit reports that and stops, rather than reviewing a nearby revision.
- Findings about work owned by another active `finch-work-claim:v1` claim are reported to the coordinator, not fixed.
