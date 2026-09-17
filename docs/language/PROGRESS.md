# Full-spec language implementation — epic tracker

Read this file first when resuming this effort cold. It is the "where are we" view; it does not
duplicate the plan itself (`IMPLEMENTATION_ROADMAP.md`) or per-issue ownership
(`finch-work-claim:v1` comments on each issue — see `CONTRIBUTING.md` and
`.agents/skills/finch-backlog/references/work-claims.md`).

## Where this lives

- **Worktree:** `/Users/shammah/repos/finch-language-full-spec`
- **Branch:** `language/full-spec-implementation`, tracking `origin/main`
- **Integration model:** this branch is the long-lived integration point for the whole M0–M5
  program. Individual roadmap issues still go through the normal tiered process
  (`finch-implement-ticket`: claim, isolated worktree, review, gate stages) and merge *into this
  branch*, not `main`, one issue/wave at a time. `main` receives one PR from this branch once M0–M5
  are done. This preserves per-issue review (Tier 3 for anything touching IR/verifier/checkpoints,
  per `SKILL.md`) instead of trading it away for a single unreviewable mega-diff — flagged once to
  Shammah 2026-09-17; his call to override if he wants a single final diff instead.
- **Plan of record:** `docs/language/IMPLEMENTATION_ROADMAP.md` (milestones, rules, exit criteria)
  and `docs/language/FINCH_LANGUAGE_DESIGN.md` (the spec itself). This file tracks status against
  that plan; it is not a second plan.

## How to resume

1. Read this file.
2. `git -C /Users/shammah/repos/finch-language-full-spec log --oneline origin/main..HEAD` for what's
   landed on the integration branch since main.
3. For whichever issue is "in progress" below, check its GitHub comments for the current
   `finch-work-claim:v1` (worker, branch, base commit, scope) — that comment, not this file, is the
   authority on who owns it and from what base.
4. Run the focused gate for the milestone you're touching (see each milestone's `AGENTS.md`-listed
   test command in `IMPLEMENTATION_ROADMAP.md`'s "Scoped language-agent packet" section) before
   doing anything else, to confirm the branch is where this file says it is.

## Milestone status

Issue state pulled live via `gh issue view` on 2026-09-17; re-pull before trusting it after any gap.

### M0 — establish the compiler boundary — mostly already true

| Issue | Title | State | Status here |
|---|---|---|---|
| #673 | shared compiler boundary | CLOSED | Done — `finch-language` facade exists (`compile`/`compile_with_functions`), phase types (`Parsed`, `Elaborated`, `FunctionCertified`, `ModuleSealed`, `ModuleVerified`) are real, private-field Rust types in `finch-vm-core/src/construction.rs`. |
| #65 | single-pass span-preserving frontend AST | CLOSED | Done. |
| — | move cross-frontend conformance tests to the compiler boundary | n/a (not its own issue) | **Done this session.** `vocabulary/language/conformance/core.json` gained a `level`/`stresses` schema; `crates/finch-vm/src/runtime.rs` gates the suite on `ACTIVE_CONFORMANCE_LEVELS` (currently `["core"]`) with a guard against the filter silently excluding every case. 6 new composition fixtures added (closure×record, list×record, option×record, closure×variant, closure×result, option×list). Verified: `cargo test -p finch-vm --lib core_language_conformance_fixtures_match_across_frontends` — green. |

M0 exit criterion ("adding a shared semantic node does not require private implementations in both
frontends") reads as already met by the closed issues; the leveling addition is the one M0 line
item (`:113`) that wasn't built yet, and now is.

### M1 — semantic kernel and published modules — not started

| Issue | Title | State |
|---|---|---|
| #66 | typed modules, immutable interfaces, decentralized imports | OPEN |
| #67 | dependency-driven semantic compiler scheduler | OPEN |
| #68 | bounded directional inference, interface publication | OPEN |
| #674 | ownership, placement, deterministic drop, safety profiles | OPEN — **blocked on a spec gap, see below** |
| #675 | coherent text and sequence values | OPEN |
| #677 | named tests, typed matchers, fixtures, mocks | OPEN |
| #86 | scripting ergonomics / model-repair metrics gate | OPEN |

**Open finding, not yet resolved:** `FINCH_LANGUAGE_DESIGN.md:2400` requires a shared conformance
corpus expressing every ownership behavior in both CoLisp and Co-Forth. The Co-Forth surface is
given (`:1620`, `consume-value`/`borrow`/`guarantees pure`/`suspends`); no CoLisp surface for
*per-parameter* ownership mode is documented anywhere in the design doc (only CoLisp's *heap
allocation* ownership syntax — `new unique`/`new shared`/`share`, `:2236` — a different feature).
The paired fixture #674 needs cannot be authored until this is resolved. Either Shammah writes the
missing CoLisp section, or it's logged as a blocking note on #674 for whoever picks it up. Not
resolved as of this write-up.

### M2 — concepts, effects, metaprogramming — not started, highest risk

#78 CTFE kernel · #79 hygienic macros · #80 parametric generics · #81 concept evidence/packs ·
#82 record rows/schema derivation · #83 effect row · #84 typed exceptions · #85 namespaced attributes.
All OPEN. This is where the actual Bend-comparison "LAWS" mechanism becomes real —
"Implement operator evidence and certified algebra laws through concepts" (`:155`) — and where
#677's test/fixture work completes "over concepts and injected effects" (`:159`). Most
interdependent milestone; least parallelizable; matches Shammah's own read that the full
parser/compiler will take a while here specifically.

### M3 — resumable execution — not started

#93 `ResumableExecution` · #94 scheduler policies · #97 ranges/streaming. All OPEN.

### M4 — self-hosting — not started

#90 Runtime/Application ABI freeze · #99 finchc AOT · #101 native benchmarks · #102 self-host. All OPEN.

### M5 — native/accelerator backends — not started

#676 verified tensor/accelerator kernel substrate. OPEN. Gated on M1–M3 semantics being stable
enough to validate native output against (translation-validation style equivalence, per `:203`).

## Conformance-suite state (the day-to-day artifact)

`vocabulary/language/conformance/core.json` — currently 46 cases, all `level: "core"` (default),
all active and passing. This is the day-to-day signal: as each M1+ issue lands, add its paired
fixtures here at a level tag matching the milestone (e.g. `"ownership"` once #674 has a resolvable
CoLisp surface), and add that level to `ACTIVE_CONFORMANCE_LEVELS` in `runtime.rs` only once the
feature is actually implemented. A case can and should be authored *before* its level goes active —
that's the mechanism for catching a spec contradiction at authoring time instead of at
re-architecture time (the original ask that started this epic).

## Next action

Two independent options, either is a reasonable next pick:
1. Resolve the CoLisp ownership-parameter-syntax spec gap (blocks #674's fixture, and #674 is a
   stated M1 prerequisite for closures/suspension/FFI representation).
2. Pick up #66, #67, or #68 (no known blockers) via the normal `finch-implement-ticket` flow, in
   its own claimed sub-worktree, merging into this branch when done.

Run `scripts/ticket_triage.py`/`scripts/ticket_poset.py` over the M1 issue set to get value/cost/
unblocking scores before picking, per the backlog skill's queue rules, rather than picking by feel.
