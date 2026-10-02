# Implementation task packet

Give a collaborator enough context to act safely without making them reconstruct the project plan. Keep the packet proportional to the delegated change.

Send the packet, not the coordinator's conversation history. Include only the files, interfaces, base revision, constraints, and proof relevant to the worker's bounded scope. Do not assign duplicate investigations or overlapping file or semantic scopes. Ask for conclusions and actionable diagnostics, not raw exploration logs, and keep independent coordinator work moving. Same-ticket repair uses the existing worker (`resume_from`), not a second packet. `$implement` packets share one byte-stable header; unique issue fields come last ([prompt cache](execution-efficiency.md)).

## Minimum packet

This is the worker's literal prompt. Include, plainly, with no rationale attached:

- Item and accepted outcome: user-facing, refactoring, deletion, or downstream-enabling work.
- Compact [solution contract](solution-contract.md) or equivalent accepted behavior, already authored — never ask a non-`strong` worker to write its own.
- Repository, branch/worktree, starting commit, and exact allowed files.
- For a backlog wrapper, the portable ASCII recovery locator (`remote-name:refs/...`), checkpoint-push authority, and terminal ref-disposition owner, deletion authority, and removal trigger. If recovery or push authority is absent, state that work remains local-only and cannot survive loss of the machine; do not describe it as crash-recoverable. Push authority never implies delete authority.
- Dependencies, known risks, and work that must remain untouched.
- Required regression and outcome-appropriate proof: user-visible, equivalence/integration/dependency, reference/reachability plus tests, or usable downstream seam.
- For a staged replacement: current stage, integration switch, predecessor-deletion owner, immediate successor, temporary-coexistence removal trigger, and proof that closes the parent outcome.
- When architecture is in scope: subsystem facade, private internals, dependency direction, co-located context/tests, boundary integration checks, and the [shape questions](engineering-judgment.md) the worker must not collapse.
- When the repository has opted into [boundary-load evidence](boundary-load.md): the primary subsystem ID; `single_subsystem` or `cross_subsystem` scope; whether inspected paths are `not_observed`, `partial`, or `complete`; and the named collector to use. Never imply complete coverage merely because a harness produces some tool events.
- Who owns implementation, review, integration, and unresolved decisions.
- Explicit permission or prohibition for edits, prototypes, external messages, push, merge, item closure, claim termination, and cleanup.
- Model lane (`cheap` / `default` / `strong`) — never a vendor model or agent type.
- Execution boundary: repository fixtures, temporary local state, loopback services, and any specifically authorized integrations; `repository-only` when the checkout and its tests suffice.
- Context boundary: start the worker without inherited conversation history (`fork_turns="none"` when available) unless a named dependency requires a small, explicit excerpt.
- Expected handoff: commit, changed files, **the gate matrix** (below), requested lane, model actually selected, failures, residual risks, and next action.

DO NOT make the handoff repeat the item or pull-request history. Link durable evidence and report only the change, verification, remaining risk, ownership, and next action.

## Name a gate stage, never a recipe

- DO name a gate *stage* the consuming repo already has (`scripts/factory/gates <stage>`, or whatever that repo documents in `AGENTS.md`) plus any test filter.
- DO NOT recite which checker to run, in which order, with which wrapper — that recitation drifts from the scripts and is how two workers end up running different things and calling both green.
- DO include the gate matrix in every handoff. List the stages the scope required and what each returned:

```text
gates docs   ok 3/3
gates arch   ok 4/4
gates test   ok 3/3 (filter: the package this change touched)
gates ci     not run — no workflow or script change
```

A worker that touched a subsystem facade and never ran the architecture stage has not finished, whatever its summary says.

An opted-in boundary-load record is completion evidence, not a readiness or authority field. The packet may request it only when the repository has a valid manifest; absent harness telemetry is `not_observed` and does not block otherwise-complete work.

- Authority comes from the responsible person or controlling instruction, not from this packet, a role name, a status label, review, or CI. Tooling is advisory mechanical lint, never an authority engine.
- DO say so and ask, rather than manufacture a proof structure, when authority is unclear.
- DO keep the packet short for a small local edit. Add isolation, concurrency, rollout, or recovery detail only when the actual risk requires it — every process step must demonstrably reduce defect risk or improve shipping confidence at a cost proportional to the change; otherwise remove it.

## Model lane

Name a lane, never a vendor model or agent type. Other people will open this ticket in a different harness.

| Lane | When |
|------|------|
| **cheap** | `$implement` and the cheap-model preflight below passes |
| **strong** | `$review`, an independent verifier, `$implement` at tier 3, or contract-authoring in `$backlog-grooming`/`$coordinate` |
| **default** | every other `$implement` |

- Pick the lane from role, risk tier, and the preflight below. Cost does not pick a model lane; it ranks the queue.
- A cheaper model may draft or implement only when the packet could state the full contract in a page: explicit acceptance criteria already recorded on the item, no authority, credential, security, persistence, wire-format, concurrency, process-lifecycle, or release impact, and allowed files and proof named exactly. Suitable work is mechanical prose, comments, formatting, narrowly scoped test-only edits, and small flagged repairs.
- Give that worker the same packet shape as any other.
- DO NOT let a worker running below `strong` lane author or rewrite a solution contract, however small, when one is missing or thin. Stop and escalate instead of improvising, the same as any other higher-risk surface: a behavior change, an ambiguity, a failing pre-existing gate, a scope mismatch, or a missing or thin contract. This applies to every non-`strong` lane, not only `cheap`. Escalation reports back to the dispatcher, which runs the `strong`-lane authoring step and resumes the same worker via `resume_from` — not a second packet.
- The coordinator or designated reviewer still owns triage, acceptance, review, integration, and every external message. A cheap worker's success signal is never proof a ticket is safe or complete.
- `$review` is independent perspectives and verification, not a bigger model. Run the protocol on the model this session has.

### Resolving a lane to an actual model

- DO resolve the packet's lane to a concrete model before calling the native delegation mechanism — a lane recorded in a packet and never resolved has no effect. `$backlog-loop` and `$coordinate` do this resolution; see the harness doc.
- DO load `.agents/harnesses/<active harness>.md` for the exact per-harness mechanism (an agent-type definition file carrying the model choice, referenced by lane name at dispatch time).
- The consuming repo's `AGENTS.md` may additionally map lanes to a spawn slug this harness understands. This kernel does not choose or ship a vendor model itself.
- DO NOT silently substitute when the harness cannot select the requested lane. If the named lane's agent definition doesn't exist yet, inherit the current model and report `requested <lane>, ran <actual>`.
