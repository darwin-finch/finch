---
name: review
description: Independently review the candidate that will land from risk-derived perspectives, verify each finding with a separate reviewer, and decide integration readiness. May split or stack work and file follow-up tickets. Use for code review, PR review, or after an implementation pass. Use when the user runs /review.
---

# Review

Review the exact candidate that will land, not a moving branch. This is the only skill that lands. Derive perspectives from the actual diff. Open [review-protocol](../../references/review-protocol.md) sections as needed (findings, convergence, land), not as a preamble. Open [engineering judgment](../../references/engineering-judgment.md) only for the shape perspective. Load the tracker from [`.agents/binding`](../../references/load-binding.md) before applying a land rule. Handoff: verdict, findings, reviewed tip. No recap.

Do not use this skill for lightweight prose edits that carry no behavioral claim.

## Required inputs

Establish before review:

- base and exact candidate commit IDs;
- intended outcome and acceptance gates;
- changed files and semantic authority;
- named regression and why it fails at the base, for a fix;
- applicable repository instructions and design decisions, including any seam the contract told compression to leave.

If the candidate changes during a round, discard that round and restart at the new exact revision.

## Workflow

1. Inspect `git diff <base>...<tip>` and select every relevant perspective from the protocol. **Test quality is always selected. Shape is selected when the diff touches models, persistence, public formats, or module boundaries.**
2. Give each selected perspective an independent pass. Do not seed one reviewer with another reviewer's conclusions. Review is the protocol, not a model pick.
3. For each candidate finding, use a separate independent verifier whose task is to test the finding and actively seek counterevidence in guards, preconditions, callers, and platform contracts. Verifier packets use lane `strong` ([task-packet](../../references/task-packet.md)); inherit and report if this harness cannot select it.
4. Label verified defects `CONFIRMED`, unresolved concerns `PLAUSIBLE`, and disproven recurring concerns `SETTLED` with their counterevidence and validity premise. Only confirmed findings block integration.
5. Repair confirmed same-contract product findings narrowly, rerun affected tests, freeze the new exact commit, and begin a fresh round. Test-only gaps become owned follow-ups; they do not open a further round.
6. Converge when a round's confirmed findings are each either a repaired product defect or a recorded test-only gap. If the patch keeps growing, repeats the same failure class, or stops making meaningful progress, split or stack it rather than hiding findings.
7. File new work discovered in review — a missing hole, a third copy of a pattern, a bug in the same area — through `$ticket-creation`. Do not silently widen the candidate.
8. Record the exact reviewed commit, perspectives, findings, verification scenarios, tests, residual risks, requested lane, model actually selected, and verdict next to the change.

Split only independently provable work. Stack when each intermediate state is safe to land and the whole would not get a real review. Large diffs attract the same number of findings as small ones.

## Non-negotiable boundaries

- A green test suite is not live-service, economic, access-control, or distributed-systems conformance unless it exercises that boundary.
- Run candidate code only within the execution boundary stated by its task packet.
- Do not mutate the frozen candidate from a reviewer context.
- Do not integrate with an unresolved confirmed finding.
- Land only from this skill, per the loaded binding's land rule.
- Review evidence belongs to the exact commit; production edits invalidate it.
