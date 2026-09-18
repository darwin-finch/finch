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

## Spec review findings (2026-09-17, ~half the doc read directly, rest verified by targeted grep)

Not exhaustive — flag anything found later here rather than assuming this list is complete.

1. **RESOLVED 2026-09-17, by changing the spec rather than the implementation.** `! pure` was
   originally spec-invalid while the entire shipping implementation and all 46 `core.json` fixtures
   used it exclusively — a real spec/implementation divergence. Resolution, reached while unifying
   the effect-row and predicate-clause syntax into one `!`-introduced list (see the design-pass log
   below): `pure`/`total`/`deterministic`/`nothrow`/`throws`/`suspends` all now live inside the same
   `!`-list as capability effects, distinguished by shape rather than by a separate `guarantees`
   clause. Under that reading `! pure` was correct all along — the classic `guarantees pure` clause
   is what's retired, not `! pure`. **No implementation migration needed**; the existing parser and
   all 46 fixtures already match the corrected spec. `guarantees` is retired as a keyword throughout
   the document (propagated to every example, including ones predating this session).
2. **"Initial module layout" (`:4393-4414`) is stale.** Names `src/vm/*.rs`, `src/coforth/frontend/`,
   `src/lisp/frontend/` — the pre-extraction layout. Actual layout (#673/#65/#667/#670, all closed):
   `crates/finch-vm-core/`, `crates/finch-colisp/`, `crates/finch-coforth/`, `crates/finch-language/`,
   `crates/finch-vm/`. Cheap fix, no decision needed — just wrong and should be corrected or marked
   historical.
3. **CoLisp per-parameter ownership syntax has no worked example** — see M1/#674 row above.
   **Drafted 2026-09-17** (`FINCH_LANGUAGE_DESIGN.md`, end of "Co-Forth parity and IR
   verification"): `consume-value` (scalar), `take`/`borrow` (declarations, tied to the existing
   `retain`/`inspect` use-after-move example), plus cross-references to the paired examples that
   already existed elsewhere for `Unique`/`Shared` construction and drop/guards. Clearly marked as
   draft, not a decision — Shammah still needs to accept, amend, or reject it.
4. **`borrow-mut` has no worked declaration example, in either syntax, anywhere in the document** —
   found while filling #3. Blocked on naming an in-place mutation primitive and its effect-row
   spelling; every record operation currently in the spec is immutable/functional-update. Needs a
   decision from Shammah, not a drafted guess — logging it rather than inventing one.
5. **`Shared<T>`'s atomic ordering is unspecified, and the stated general default is very likely
   wrong for it specifically — found 2026-09-17 in a buildability-focused audit. **Half-resolved
   the same day.** Strong-count ordering is now specified in "Library ownership carriers and the
   compiler lifecycle kernel" (`relaxed` retain, `release` decrement, `acquire` fence gating the
   destructor only on the decrement that reaches zero — matches `Arc`'s proven scheme). Deliberately
   left open: `Weak<T>`'s count and its interaction with `upgrade` — `Arc`'s real implementation has
   genuine additional subtlety there (a compare-exchange loop, a weak count that doesn't simply
   mirror the strong count) that was not asserted by analogy alongside the part that's actually
   well-established, to avoid guessing at exactly the class of subtle concurrency bug this finding
   is about. **Needs a dedicated pass before `Weak<T>::upgrade` is implemented** — not inferable from
   the strong-count scheme now in the document.

## Plan: spec fixes first, then M1 by dependency order

### Step 0 — resolve found spec defects (do this before M1 issues generate more surface on old ground)

| Item | Tier (per `finch-backlog`) | Action | Owner |
|---|---|---|---|
| ~~`! pure` vs `guarantees pure`~~ | — | **Resolved 2026-09-17** — see findings list above. No action needed; spec now matches the shipping implementation. | — |
| Stale module layout (`:4393-4414`) | Tier 1 | Rewrite to the actual crate layout or mark the section historical/superseded-by-roadmap. No contract, no review round needed. | anyone, immediately |
| CoLisp ownership-parameter example missing | Decision needed | Shammah writes the missing worked example, or explicitly hands it to whoever picks up #674 as a stated blocker. | Shammah or #674's owner |

### Step 1 — M1 issues, dependency-ordered

Run `scripts/ticket_triage.py`/`scripts/ticket_poset.py` over #66, #67, #68, #674, #675, #677, #86
to get real value/cost/unblocking scores and a dependency-ordered wave before picking by feel, per
the backlog skill's queue rules. Known dependency shape from the roadmap text alone (not yet
poset-verified): #674 (ownership) is a stated prerequisite for closures/suspension/FFI
representation staying cheap to change, so it likely gates more than it looks like from issue
number order; #677 (tests) explicitly completes in two passes (a first pass now, gated on nothing
below, and a second pass in M2 "over concepts and injected effects").

### Step 2 — M2 onward

Follow `IMPLEMENTATION_ROADMAP.md` as written; re-run triage/poset per milestone rather than
planning M2–M5 in detail now, since M1's actual shape will change what's ready.

Each M1+ issue: normal `finch-implement-ticket` flow (claim, isolated worktree, review, gate
stages), merging into `language/full-spec-implementation`, not `main`. Paired fixtures land in
`core.json` at that issue's level tag; `ACTIVE_CONFORMANCE_LEVELS` gains that level only once the
issue is actually merged, not when it's claimed.

## Ownership/memory model — design pass log (2026-09-17, same day as everything above)

A long live design discussion with Shammah substantially extended and twice audited the ownership
section beyond the #674 scoping work above. Summary, not a repeat of the commit history — read
`git log` on this branch for the full sequence:

- `steal` replaced `take` as the ownership-transfer keyword everywhere (full-document rename,
  verified against 7 genuine non-keyword English uses of "take" that were deliberately excluded).
- `steal x: Foo` / `take x: Foo` shorthand (accept any `Owner<Foo>`, carrier inferred) — added, then
  the "no shorthand" requirement's rationale was fully reversed per explicit decision.
- Retired as redundant, explicitly: `static Owner<Foo>` as a spelled-out alternative to bare `Foo`,
  and explicit `(borrow x : Foo)` as an alternative to unannotated — both changed nothing over the
  shorter form once checked, so both were cut rather than kept "for readability."
- `Unique<T>` → `Shared<T>` by move; the reverse only via checked `try-into-unique`; `strong-count`
  (diagnostic-only, pure-but-not-deterministic) and `get-mut` (borrow-not-consume) added alongside it.
- Move-vs-copy default corrected (plain records move, `Copy` types copy — not the reverse), with the
  mechanical nuance that move and copy are physically identical for pointer-free types, and why the
  invalidation rule still earns its keep even then (decoupling from field layout; non-trivial drop
  hooks).
- `borrow-mut` deliberately excluded from the carrier-shorthand treatment (`Shared<T>` can't
  unconditionally provide `&mut T`).
- Escape-to-heap is a compile error requiring explicit `Unique`/`Shared`, never silent promotion —
  reasoned through in detail (ownership-policy ambiguity; inserts an allocation absent from source;
  is in the limit the GC-equivalent machinery the whole memory-model rationale trades away).
- Two full audits found and fixed real defects: a stale pre-shorthand-revision paragraph that still
  said carrier dispatch was mandatory, and one line-wrap bug in the `take`→`steal` rename script that
  briefly mis-renamed a genuine English sentence (caught by re-verifying the exclusion count before
  committing).
- `same-address` (identity comparison, distinct from `==`, scoped to borrows) and `match-type`
  (CTFE-time carrier introspection inside one generic body, narrowing the bound type per arm) added
  to close two more gaps Shammah found: metaprogramming needing to know the concrete carrier, and
  wanting automatic per-instantiation specialization without a second, competing declaration.

None of this is implemented — it is still all `FINCH_LANGUAGE_DESIGN.md` prose, ahead of #674's
code. Before #674 is claimed, re-read the ownership sections fresh rather than trusting this summary
line-for-line; a design discussion this size run live in conversation is exactly where something
subtle could still be wrong despite two audit passes.

**Continued the same day — further corrections and one syntax unification, found by Shammah pressing
on machine-code representability and buildability specifically, not just design coherence:**

- **Two real "not actually buildable" errors caught and fixed**, both by Shammah asking "how would
  this actually compile": the borrow/take shorthand's claimed monomorphize-or-share choice isn't free
  in general (ownership transfer across differently-sized carriers needs boxing to share; only
  already-uniform-representation carriers like `Unique`/`Shared` can share for free), and `match-type`
  forces monomorphization outright rather than merely permitting it (a shared/erased body is compiled
  against a fixed vtable shape and cannot call an operation `match-type` might reach for). Also
  confirmed `O` (the hidden carrier) is one ordinary generic type parameter sharing the same
  instantiation-job keying as any hand-written generic, not a second specialization system —
  and separately, `match-type` over an already-erased `dyn Owner<Foo>` value resolves at *runtime*
  (comparing the runtime type identity `dyn` values already carry), not CTFE — a materially different
  mechanism from the static-generic-parameter case, missed in the first pass.
- **Syntax unification, requested directly ("I don't like having two attribute dimensions"):** the
  effect row (`!`) and the separate predicate clauses (`guarantees pure`, `nothrow`, `throws A|B`,
  `suspends`) are now one `!`-introduced list, told apart by shape (request-shaped vs. closed-keyword-
  shaped) rather than by grammar. This is what resolved finding #1 above — `! pure` was correct all
  along under the unified reading, so the fix was changing the spec, not migrating 46 fixtures.
  Propagated to every example in the document, including several that predate this session (the
  `Equal<L,R>`/`Compare<L,R>` concept operations, `square`, `save-report`, `load-user`, `read-user`,
  the closure-conversion IR sketch).
- **New rule added alongside it:** writing any predicate explicitly commits the signature to the
  complete, atomic contract (both symmetric axes — throw-or-nothrow, suspend-or-not — resolved, never
  a partial hybrid) — generalizes the publication-commitment rule already required for `throws`.
  Applying it to the pre-existing `Equal`/`Compare` examples surfaced that they'd never stated
  `nothrow` despite asserting everything else — fixed in the same pass.
- **Standard construction/conversion added** (no prior general mechanism existed, only ad hoc factory
  functions): `From<T>`/`Into<T>` as an ordinary concept, always explicit at the call site — and, after
  a direct request for "explicitly marked implicit constructors," a *narrower*, compiler-*verified*
  form (the conversion's inferred effects must satisfy the same cost bar as every other automatic
  adaptation; at most one direct, non-chained candidate per call site) rather than blanket C++-style
  implicit invocation, which was considered and rejected on the same grounds as classic overloading.
- **A genuine strategic check-in, worth recording as a standing note, not just a design decision:**
  Shammah's actual goal for this whole effort is to fully specify the language well enough to set
  "an army of LLMs" building the compiler with minimal human bottleneck. Answered directly: a prose
  spec reaching zero remaining ambiguity/error isn't a realistic bar — this session found real errors
  under the best possible conditions (slow, adversarial, one decision at a time) — so what actually
  makes large-scale parallel automated implementation viable isn't prose completeness, it's (a) the
  executable fixture corpus (`core.json` and its planned growth) as the actual machine-checkable
  arbiter, and (b) keeping the existing tiered review process (`finch-backlog`, Tier 3 for anything
  authority/persistence/wire-format-shaped — a compiler qualifies) load-bearing *per change*
  regardless of how many agents are running in parallel, not skipped because agents are doing the
  writing. Recorded here so it isn't lost as just a conversational aside — it should inform how the
  M1+ work in this tracker actually gets executed once agents start picking up issues.

**Continued 2026-09-17 — caught a self-inconsistency in the records section, fixed it, and started
a running example-program corpus (`docs/language/examples/feature_tour.md`) as a second review
method alongside prose audit:**

- **Records didn't actually have a way to declare behavior.** The `@constructor`/`@property`/
  field-visibility additions from earlier the same day used the term "associated function" as if a
  record had its own inherent methods; grep confirmed that term appeared nowhere else in the
  document, and every other `implementation` block in the whole spec is `implementation X for Y :
  Concept {...}` — there was no bare form. Shammah caught this by asking directly whether records
  even have methods, recalling an earlier conversation had subsumed all record behavior into
  `concept`/`implementation`.
- **Fix:** `implementation Foo { ... }` with the concept bound simply omitted is now an *inherent
  implementation* — same declaration shape, no new mechanism. It hosts three member kinds by leading
  keyword: `constructor` (replaces `@constructor`, same enforcement — record-literal syntax becomes
  unavailable outside a constructor's own body once any exist), and `get`/`set` (replace `@property`,
  same C#-shaped getter/setter-as-field semantics, still forbidding a plain field and a `get`/`set`
  under one name). Shammah specifically flagged `@constructor`/`@property` as possibly the wrong
  spelling, citing C#/TypeScript's `get`/`set`; the attribute mechanism was dropped rather than
  patched, since attributes-as-a-second-dimension were already something he'd pushed back on earlier
  this session (the `!`-unification finding above).
- **New gap this surfaced and closed in the same pass:** inherent implementations mean an
  `operation` and a concept-dispatched operation can now share a name on one type, which the `.`
  resolution passage never addressed. Added explicit precedence — field → `get`/`set` → inherent
  `operation` → concept dispatch, inherent shadowing concept deliberately (Rust's inherent-vs-trait
  precedent) — rather than leaving it to reach implementation as an undefined collision.
- **Example-program corpus started, at Shammah's request, explicitly as future compiler test
  material:** small CoLisp programs that each exercise several features together (records/
  construction/properties; ownership + `match-type`; effects + concepts + `?`; variant + record-arm
  destructuring; an inherent-vs-concept-operation shadowing case), rather than one feature at a time.
  4 of the first 5 clusters composed cleanly as specified with no changes needed; the fifth (records)
  is what surfaced the gap above. Also surfaced, by absence rather than by writing a broken example:
  capability requests with wildcarded paths (`read{path="~/**"}`, asked about earlier this session)
  were never actually specified — only a passing "broad selectors such as workspace `**`" mention
  exists, with no grammar. Flagged, not guessed at. This is meant to keep running: the plan going
  forward is prose-audit-and-example-program in the same pass, since the records gap was found by
  trying to write code against the spec, not by re-reading it.

**Continued 2026-09-17 — the example programs immediately caught a second, worse defect than the
records one: I'd been writing CoLisp with invented syntax (`fn`, `->` for return types, `=>` before a
body) that isn't in this document anywhere.** Shammah caught it by asking directly whether Finch uses
that arrow notation. The real form, already established in "Functions and annotations"
(`square`/`save-report`) and now used consistently in the `constructor`/`get` fix above and
throughout `feature_tour.md`: `(define (name (params...)) : ReturnType ! effects body...)` — `define`,
`: T` after the parameter list closes, no separator token before the body. Also wrong: record
construction as `Foo { x: a, y: b }` (that's Co-Forth's spelling) instead of CoLisp's
`(Foo :x a :y b)` from the parity ledger, and a redundant explicit `(borrow x : Foo)` this document
already retired.

Also answered directly, since Shammah asked it precisely: `->` is not a C++-style parsing hazard in
either frontend, because both readers tokenize purely by whitespace and a small fixed reader-macro
set, never by context-dependent retokenization — a bare `->` always reads as one atom. The actual
defect was spelling, not ambiguity; noted in the doc so it isn't re-litigated.

Fixing this surfaced two more open items while writing real code against real established syntax
rather than pseudocode, both now logged rather than guessed past:
- **No confirmed call-site convention for an inherent constructor or operation.** `Account.open(...)`
  is a guess by analogy (a constructor has no receiver to call through, so it needs *some*
  namespacing), not a confirmed spelling — nothing else in the document calls one.
- **Concept dispatch is not `Type.operation(...)`.** Checked against "Every implementation has a
  stable qualified name... a call either names it with `using`, receives it through a generic
  evidence parameter, or uses one default explicitly imported" ("Generics, concepts, dispatch, and
  metaprogramming") — dispatch is a bare call resolved against evidence in scope, not
  type-qualification. `feature_tour.md` §3 corrected to `(serialize u opts)` instead of
  `User.serialize u opts`.
- **Generic-header placement on a hand-written `define`** (e.g. `<O : Owner<Foo>>` for a
  `match-type` body) has no example anywhere either — every real `match-type` example matches on a
  parameter already in scope, never shows the signature that bound it. Marked UNVERIFIED in
  `feature_tour.md` §2 rather than presented as settled.

None of these three are fixed yet — they're the next things to resolve, in that order, since the
constructor/operation call-site question blocks writing any further inherent-implementation example
cleanly.
