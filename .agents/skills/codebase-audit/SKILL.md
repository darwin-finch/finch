---
name: codebase-audit
description: Audit the whole tree, optionally since the last completed audit/refactor checkpoint, for boundary degradation, dead code, earned abstractions, and collapsed seams. Drafts or files evidence-backed tickets; never refactors in place. Use when the user runs /codebase-audit.
---

# Codebase audit

Feature work never asks whether a module is still reached. A bugfix review never sees the third copy of a helper. Run this skill occasionally, or when asked — not as part of every loop ([eligible work](../../references/queue.md#eligible-work)) — against the whole tree, not against the diff of the ticket in front of you. Load the tracker from [`.agents/binding`](../../references/load-binding.md) before filing.

The questions are in [engineering judgment](../../references/engineering-judgment.md). This skill is how they get asked of code nobody is currently touching. Output is tickets via `$ticket-creation`, ranked later by `$backlog-grooming`. Do not start a refactor in place; an audit that lands a rewrite has become implementation without a contract.

When the repository has opted into [boundary-load evidence](../../references/boundary-load.md), use its historical report to choose boundaries worth inspecting. An increased dependency-implementation escape rate is an audit candidate, not a root cause. Check observation coverage first; `partial` and `not_observed` claims are not zero-escape claims and are excluded from the rate.

## Since the last completed pass

Resolve the latest reachable annotated checkpoint with `.agents/scripts/boundary_load.py checkpoint`. Checkpoints are tags named `boundary-load-audit/v1/<UTC timestamp>` and point at the integrated tree after a clean audit or after every accepted corrective ticket from that pass landed. Ignore lightweight, unreachable, or differently named tags.

Load the active binding and collect boundary-load evidence from terminal item/PR records. Give the reporter the full available history for its baseline, then run `boundary_load.py report RECORDS --since-checkpoint`; only candidates with eligible evidence newer than the checkpoint are newly actionable. If no checkpoint exists, treat this as the first pass. If evidence cannot be enumerated or remains partial, report that coverage gap and continue the structural audit without inventing zero escapes.

Walk the whole tree as this skill normally requires. Use `git diff <checkpoint>..HEAD` to prioritize modules changed since the completed pass, not to exclude unchanged modules. Dedupe every finding against open and recently closed tracker items so a checkpoint never becomes permission to file the same problem twice.

## What to look for

Walk modules, not files. For each subtree that claims to be a boundary (a directory with `AGENTS.md`, a package, a crate, a feature folder):

- **Reached?** Is anything outside still calling this, through the facade it claims? Evidence is references, tests, and generated interface files, not "it looks important." Unreached code becomes a deletion ticket naming the exact symbols and the search that found no callers.
- **Loadable?** Can an agent changing this module work from its code, its instructions, and the *interfaces* of its dependencies — or must it open implementations? If implementations leak, the boundary is fiction; ticket the facade, don't pretend.
- **One home?** Is a fact represented in two places without a stated derived-from relationship?
- **Earned abstraction?** Has the same shape now appeared three times with real tension (call sites that drift, fixes applied to one copy and not the others)? Rule of three is a warning, not a mandate — the ticket should show the three copies and the cost of leaving them. Three calls to a bad `Utils` helper are not an earned abstraction.
- **Nonsense states?** Booleans and strings that can represent combinations the product forbids. Names that are job titles (`Manager`, `Utils`) with no job. Validation sprinkled down a stack instead of parsed at the edge.
- **Collapsed seam?** Was a durable record flattened (boolean for a known enum, string for an identity, one-to-one for a relationship the product already treats as many) in a way that a *currently accepted* next feature would have to migrate? Cite that feature. Hypothetical plugins are not seams.
- **Chunkability?** Functions or types that cannot be held in working memory, globals that couple otherwise independent pieces, directories that import in a cycle.
- **Leftover scaffolding?** Compatibility layers, feature flags, or adapters with no owner, no successor, and no removal trigger.

## What not to look for

- Style, import order, comment tone — unless a consuming repo's `AGENTS.md` says they are load-bearing.
- Speculative "this could be a framework." That is the failure mode this skill exists to avoid.
- Issues already on the tracker. Dedupe; add evidence to the existing item.

## How far to go

An audit is a survey, not a proof of every finding. Each candidate gets:

- the location and the claim ("this is dead", "these three want one interface", "this boolean is the invites hole");
- the cheapest evidence that would falsify it (a caller, a test, a ticket that is *not* actually accepted);
- a suggested ticket title and the draft shape from `$ticket-creation`.

For a boundary-load candidate, also state the compared windows, eligible claim counts, observed escapes, a bounded architectural hypothesis, preserved behavior/API invariants, and the expected measurable effect. Falsify poor ticket scope or legitimate cross-subsystem work before blaming the facade.

Stop when additional walking would only produce more of the same class already ticketed, or when the user-stated budget is exhausted. Prefer a short list of high-reversal-cost items over a catalog of nits.

When the invocation explicitly says to file or publish the audit findings, that is batch publication authority for tickets that satisfy the evidence and draft requirements above; publish them through `$ticket-creation` and report every created item. Otherwise show the drafts before publishing. The audit does not claim, implement, or merge.

Do not advance the checkpoint merely because an audit ran or tickets were filed. After a clean audit, or after all accepted corrective work from the pass is integrated and post-change evidence is recorded, an explicit request to complete the pass may create an annotated tag at current `main`. Use UTC `boundary-load-audit/v1/YYYYMMDDTHHMMSSZ`, include the audit and corrective item identities in the annotation, and verify the tag resolves to the intended commit. Pushing the tag requires explicit push authority. Never move or reuse an existing checkpoint tag.
