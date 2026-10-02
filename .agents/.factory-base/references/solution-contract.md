# Compact solution contract

A solution contract is a short shared description of what will change and how the team will know it worked. It prevents implementation from outrunning product and engineering decisions. It is not an ownership record or an approval engine.

Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost proportional to the change; otherwise remove it. A compact solution contract for an obvious fix may fit in one comment. Expand it only when the change has corresponding risk.

## Required content

Record:

- item and accepted outcome, which may be user-facing, deletion, refactoring, or enabling work;
- observed failure or motivating example;
- intended behavior and explicit non-goals;
- current architecture, target architecture, affected boundaries, and allowed files;
- important correctness, security, compatibility, or lifecycle invariants;
- outcome-appropriate proof: fail-before/pass-after and user-visible behavior for a bug fix; refactor equivalence, integration, and dependency-boundary proof; deletion reference/reachability plus tests; a usable downstream seam for enabling work;
- for replacement work, the replacement, integration switch, and predecessor-deletion stages;
- when the outcome requires a refactor or migration: the stack as separate landings (tests that pin current behavior, then the refactor/migration, then the feature); the parent stays open until that stack is on main;
- any temporary coexistence owner, immediate successor, removal trigger, and deletion proof;
- intended subsystem facade, private internals, dependency direction, co-located context/tests, and boundary integration tests when architecture is in scope;
- for a refactor prompted by [boundary-load evidence](boundary-load.md): the triggering historical observations, falsifiable architectural hypothesis, preserved behavior/API invariants, and expected measurable effect on dependency-implementation escapes;
- when the change touches a durable record: the [cost-of-reversal](engineering-judgment.md) answers — current shape, target shape, known next feature the target must not paint over, and the framework being refused;
- responsible owner for unresolved decisions; and
- rollback or deletion plan.

The contract should be readable without a protocol decoder. Link supporting evidence normally. Tooling is advisory mechanical lint, never an authority engine.

For an item discovered by `$review` or `$codebase-audit`, build the contract from the originating finding's own evidence (the concrete failure, the simplest coherent correction, the regression, the affected invariant) rather than re-deriving it from scratch.

## Change control

If implementation discovers a materially different outcome, boundary, authority need, or risk, stop and get the responsible person's decision before expanding production work. Small clarifications that do not alter those things can be recorded directly without restarting the whole process.

## Review and completion

Review the current implementation against the contract, including the shape questions when they apply. Keep same-contract corrections in the current change and split only genuinely separable outcomes with a clear owner and proof. The contract is met only when the reviewed result is integrated on current main and the promised evidence passes. A staged replacement remains incomplete until the predecessor and unwanted compatibility machinery are deleted.
