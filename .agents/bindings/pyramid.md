# Binding: Pyramid

Pyramid is a live deployment of hierarchical work items. `pyr` is the only backlog surface. GitHub issues, a seed roadmap, and conversation history do not mirror live state.

This binding substitutes tracker verbs when `.agents/binding` sets `tracker: pyramid`. It does not fork the kernel. Until a consuming Pyramid repo migrates, its own skills remain the ones that run.

## Mapping

| Kernel | Pyramid |
| --- | --- |
| item | Node on the live deployment (`pyr show <ref>`) |
| claim | `pyr own` / `pyr disown`, plus coordinator-assigned file and semantic scope recorded with `pyr say`. Account-level ownership is not a fence between two workers sharing one service account. |
| blocker | `pyr block` / `pyr resolve-blocker` — a relationship, not a note |
| score | **Do not invent the four-axis formula.** The deployment does not store it. `pyr ready` is the dependency-ordered frontier (blockers resolved, then relationship and reference-key order). The hole for ranking is *points*: scarce internal currency so dependencies have a cost. When points exist and are stored on the item, they replace this binding's ranking function; they do not remove readiness. |
| ready | Domain-enforced (`ClaimReady`): criteria, no unresolved blockers, no open work children, a work kind. Absent from `pyr ready` means repair the record (`pyr criteria`, `pyr block`, `pyr say`), not a score bump. |
| packet | Recorded on the item; same fields as [task-packet.md](../references/task-packet.md) |
| land | Fast-forward `main` to the exact reviewed series tip. No merge commit, no server-side squash or rebase after review — those would change the reviewed identity. |

When a consuming repository opts into [boundary-load evidence](../references/boundary-load.md), store the separate `software-factory/boundary-load-evidence/v1` artifact with `pyr evidence`; keep the URI and label on the item. It is terminal evidence, not points, readiness, or ownership state.

## Verbs

- `pyr ls <query>` / `pyr show <ref>` / `pyr history <ref>` — inspect
- `pyr new <parent-ref> <title>` — create under the subsystem that owns the area
- `pyr criteria <ref> ...` — acceptance checklist
- `pyr say <ref> ...` — durable notes (contracts, evidence links, scope)
- `pyr own` / `pyr status <ref> in_progress` / `pyr status <ref> review` / `pyr accept <ref> <why>`
- `pyr evidence <ref> <kind> <uri> <label>` — terminal evidence at the exact revision
- `pyr block` / `pyr resolve-blocker`

Admission is domain-enforced: a node that cannot be honestly claimed refuses the command with the reason named. Fix the record, not the fence.

## Scripts

Do not run `ticket_poset.py` or `ticket_triage.py` against Pyramid. `pyr ready --json` is that report, already filtered to claimable work.

## Land rule

The integration candidate is the exact tip of the linear series of per-feature squashed commits. Review that tip. Advance `main` by fast-forward. If CI repairs change the candidate, fold each repair into the commit for the feature it fixes and repeat exact-revision review.

## Points (the ranking hole)

Pyramid rejected a priority score on the frontier because the frontier is an ordering the domain already computes, and because re-scoring current facts was a source of churn. Points are a different mechanism: they cost internal work the way money costs external work, which is how you see that a "cheap" ticket pulling on three teams is not cheap.

Until points are stored on items and spent by the loop, this binding uses the domain frontier and does not compute `(value × certainty × (1 + unblocking)) / cost`. The kernel's readiness rules still apply. The kernel's default formula still applies to GitHub-binding repos.
