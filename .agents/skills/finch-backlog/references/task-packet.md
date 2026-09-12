# Implementation task packet

Give a collaborator enough context to act safely without making them reconstruct the project plan.
Keep the packet proportional to the delegated change.

## Minimum packet

- Issue and accepted outcome, including whether it is user-facing, refactoring, deletion, or
  downstream-enabling work.
- Compact solution contract or equivalent accepted behavior.
- Repository, branch/worktree, starting commit, and exact allowed files.
- Dependencies, known risks, and work that must remain untouched.
- Required regression and outcome-appropriate proof: user-visible, equivalence/integration/
  dependency, reference/reachability plus tests, or usable downstream seam.
- For a staged replacement: current stage, integration switch, predecessor-deletion owner, immediate
  successor, temporary-coexistence removal trigger, and proof that closes the parent outcome.
- When architecture is in scope: subsystem facade, private internals, dependency direction,
  co-located context/tests, and boundary integration checks.
- Who owns implementation, review, integration, and unresolved decisions.
- Explicit permission or prohibition for edits, prototypes, external messages, push, merge, issue
  closure, claim termination, and cleanup.
- Expected handoff: commit, changed files, **the gate matrix** (below), failures, residual risks,
  and next action.

## Name a gate stage, never a recipe

A packet names `scripts/factory/gates <stage>` and, for the rust stage, the test filter. It does
not recite which checker to run, in which order, with which wrapper: that recitation drifts from
the scripts and is how two workers end up running different things and calling both green.

A handoff without its gate matrix is incomplete. List the stages the scope required and what each
returned:

```text
gates docs   ok 3/3
gates arch   ok 4/4
gates rust   ok 3/3 (filter: context:: generators::claude)
gates ci     not run — no workflow or script change
```

A worker that touched a subsystem facade and never ran `gates arch` has not finished, whatever its
summary says.

Authority comes from the responsible person or controlling instruction, not from this packet, a
role name, a status label, review, or CI. Tooling is advisory mechanical lint, never an authority
engine. When authority is unclear, say so and ask rather than manufacturing a proof structure.

Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost
proportional to the change; otherwise remove it. For a small local edit, the packet should be short.
Add isolation, concurrency, rollout, or recovery detail only when the actual risk requires it.
