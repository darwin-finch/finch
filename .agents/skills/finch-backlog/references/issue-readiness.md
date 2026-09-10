# Lightweight issue readiness

A lightweight readiness check answers one question: is the outcome clear enough to begin the next
kind of work safely? It is a human judgment supported by evidence, not a state machine.

Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost
proportional to the change; otherwise remove it. Tooling is advisory mechanical lint, never an
authority engine.

## Ready-to-implement checklist

Before production work, record:

- the accepted outcome and a concrete example of the current failure or motivating need;
- the smallest acceptable behavior change and explicit non-goals;
- affected files or boundaries, likely risks, and rollback approach;
- the named regression or proof, including fail-before/pass-after for a bug fix;
- required outcome-appropriate proof: user-visible behavior, refactor equivalence/integration,
  deletion reachability plus tests, or a usable downstream seam;
- the person accountable for unresolved product or scope decisions; and
- a conflict check against active work in the same files or semantics.

For a small documentation fix this can be a paragraph. More detail is justified only by real
correctness, security, persistence, compatibility, or rollout risk. A script can point out missing
text, but it cannot decide that an issue is ready or authorize work.

## Practical states

Use only the state needed to make the next action clear:

- `UNSPECIFIED` (`NEEDS_SPECIFICATION` in older records): the outcome or a required decision is
  missing. Name the question, decision owner, and smallest investigation.
- `READY`: the outcome, boundaries, proof, and responsible people are clear enough to implement.
- `IN_PROGRESS`: claimed implementation is underway on an isolated branch/worktree.
- `REPAIR_IN_PROGRESS`: review found same-contract work that is being corrected.
- `READY_TO_MERGE`: blockers are zero, one clean review of the current candidate passed, and
  affected gates pass after integrating current main.
- `BLOCKED_EXTERNAL`: a named external condition prevents the next action. Record the condition,
  who controls it, what is preserved, and what can proceed independently.
- `INFEASIBLE`: evidence shows the outcome cannot be delivered under accepted constraints. Record
  the contradiction and the smallest constraint change that would help.
- `DECLINED`: the accountable owner chooses not to pursue a feasible outcome. Record the reason,
  consequences, and preservation plan.
- `SUPERSEDED`: the owner replaces the outcome with genuinely separable, explicitly owned work;
  inherited acceptance obligations remain visible until proven.
- `COMPLETE`: current-main merge and outcome-appropriate evidence are present, and issue,
  claim, cleanup, and follow-up accounting are truthful.

These labels communicate status; they do not grant edit, merge, closure, or disposition authority.
Do not encode every possible transition. When facts change, record the new state, evidence, owner,
and next action in plain language.

## Candidate and finding separation

Issue readiness does not imply that a patch is correct. Candidate review asks whether the
identified current candidate meets the contract. Findings separately carry confidence, severity,
locality, obligation, and
lifecycle. Neither finding count nor worst severity automatically changes readiness or closes work.

Completion is based on the integrated result, not paperwork: verify the reviewed change on current
main, run the affected checks, and collect the proof appropriate to the accepted outcome.
