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

**Continued 2026-09-17 — a live design conversation with Shammah about surface aesthetics ended in a
real, shipped syntax change: `let`/binding lists now use `[...]`, not doubled `(( ))`.**

- **Path here:** Shammah said plainly he's more drawn to C-family syntax than Lisp's, specifically
  citing "extra `()`s everywhere and no distinguishing blocks from tuples" and noting the type system
  already compromises pure homoiconicity, so treating uniform-parens as untouchable was inconsistent.
  Landed on Clojure's bracket-variety convention (`[]` for sequences/bindings, distinct from `()` for
  calls) rather than a full C-syntax rewrite — real, heavily-trained-on precedent, not invented
  syntax, which matters given the explicit LLM-usability constraint Shammah added mid-discussion.
- **Naming-convention side-thread, same conversation, resolved differently than first proposed:**
  considered a hard PascalCase-types/PascalCase-methods/camelCase-locals rule (real .NET convention).
  Shammah's gut reaction to `Account.Open()` was right — revised to PascalCase-types/camelCase-
  everything-else, which is actually *more* universal (Java/JS/TS/Swift/Kotlin agree; C#'s
  PascalCase-methods is the outlier) and simpler (two buckets, not three). Not yet written into the
  spec — this session's actual syntax work stayed on brackets; casing is still open.
- **Real collision caught against shipped code before committing to `[]`:** `reader.rs` already
  treats `[...]` as an unconditional JSON-array span parsed by `serde_json`. Resolved as Shammah
  proposed once the shape was checked — `[...]` is a strict superset of JSON-array syntax: content
  that parses as valid JSON is a JSON literal exactly as today (handled by the JSON sub-parser,
  untouched); content that doesn't falls back to ordinary form-by-form Lisp reading, the same
  tokenizer `(...)` already uses. Every JSON scalar already had an existing Lisp-atom reading in this
  reader (`true`/`false` already aliased to `#t`/`#f`, `null` to `nil`), so JSON is genuinely a
  subset, not a second case needing separate fallback logic.
- **Self-caught error, same day, same addition:** the first version of this rule said `,` becomes "an
  insignificant separator... inside `[...]` specifically." Checking that against this document's own
  quasiquote example (`` `(let [r ,body] ...) ``, in "Definitions and signatures") shows it's wrong —
  `,body` is load-bearing unquote there, and that example already lives inside a `[...]` binding
  vector. Corrected: comma is never blanket-optional; a `[...]` span is either fully valid JSON
  (comma is JSON's own separator, a different sub-parser entirely) or it isn't (comma keeps its one
  existing, unconditional meaning as unquote, exactly as everywhere else) — never a blend of both
  rules in one span. Caught by Shammah asking "could `,` be optional in a list?" before the rule was
  ever exercised against a real example rather than after.
- **Executed, not just decided:** every `let` in `FINCH_LANGUAGE_DESIGN.md` (13 occurrences) and
  `feature_tour.md` (5, including one multi-binding case) converted from `(let ((n 10)) ...)` to
  `(let [n 10] ...)` / `(let [a 1 b 2] ...)`. Also closed a real, separate, previously-unfound gap
  while writing the rule: no CoLisp tuple *construction* literal existed anywhere before this —
  `[1 2 3]` now constructs `tuple<int,int,int>`, the same `[...]` form used positionally.
- **Not yet done:** the same nested-list problem exists in parameter lists (`(square (x : int))` —
  list-of-one-list for a single parameter, for the same structural reason `let` had it). Diagnosed in
  conversation — the fix isn't a straight copy of `let`'s, since parameter entries have variable
  token count (an optional ownership keyword) unlike `let`'s always-exactly-two-forms bindings, so
  full flattening would be ambiguous. The proposed fix instead re-brackets only the outer list to
  `[...]`, keeping each parameter's own `(...)` grouping: `(define (square [(x : int)]) : int ...)`.
  Proposed, not yet written into the spec or applied to existing examples — pending confirmation.

**Continued 2026-09-17 — attempted the parameter-list `[...]` fix above, found the proposal itself
was wrong, reverted before committing, and shipped a different, real addition instead
(`ParameterSpec`/`ParamEntry`).**

- **The revert:** a scripted conversion assumed `define` has a separate "params-only" wrapper list
  distinct from the function name, the same way `lambda` genuinely does
  (`(lambda ((x : int)) body)`). Checking `save-report`'s real multi-parameter shape —
  `(define (save-report (path : ...) (contents : string)) : unit ...)` — disproves that: `define`'s
  name and every parameter entry are flat siblings in one list, no separate params wrapper at all.
  The script re-bracketed the first entry's own parens instead of a wrapper that doesn't exist,
  corrupting multi-parameter signatures (mismatched `[`/`)`). Caught before commit
  (`git diff --stat` showed only the one bad file; `git checkout --` discarded it cleanly). `define`
  and `lambda` genuinely disagree with each other on this shape — real, still open, needs a
  considered fix rather than a scripted one next time.
- **`ParameterSpec`/`ParamEntry` added instead**, prompted by Shammah's point that CTFE functions
  need real structure to inspect, not raw syntax they'd have to re-parse themselves ("otherwise they
  have to re-invent the parser and a lot of other compiler machinery"). Checked first rather than
  assumed unaddressed — the document already states the right principle ("syntax values are not bare
  lists... public syntax constructors and projections") and already has one concrete instance,
  `CaptureSpec`/`CaptureEntry` for lambda captures — but nothing analogous existed for an ordinary
  parameter list. Added by reusing `CaptureEntry`'s existing before/after-name-resolution field split
  directly (syntax identifier + written annotation, then binding ID + resolved type + resolved
  ownership mode + origin) rather than inventing a second shape; `CaptureSpec.parameters` is now
  `ParameterSpec`, not a parallel description of the same list.
- **Corrected, same conversation, a related question about quote vs. quasiquote:** checked against
  the actual text ("`'form` produces `syntax`... `` ` `` is a template") rather than accepting a
  plausible-sounding "quote gives flat data, quasiquote gives the AST" hypothesis — both quote and
  quasiquote (with no unquote holes) produce the same `syntax` value; the real difference is that
  quasiquote allows `,`/`,@` holes and quote doesn't. Also corrected my own earlier answer in this
  same conversation, which had described a quoted parameter form as "a flat list of symbols" — that
  describes `syntax->datum`'s explicit, lossy output, not the default `syntax` value.

**Continued 2026-09-18 — that "quote and quasiquote produce the same value" claim from the previous
entry was itself wrong, caught by Shammah asking a pointed enough question about it, and reversed.**

- **Path here:** a long CTFE/macro-hygiene design conversation (how does a CTFE function resolve a
  quoted identifier passed through several layers of compile-time function calls; why that's not
  the same problem as "can a macro emit an already-resolved reference," which it can) led to
  Shammah noticing that what I'd been describing `'` as doing — carrying lexical scope marks
  sufficient for correct hygiene — is not what plain quote does in any real Lisp. Checked precisely:
  vanilla Scheme's `'` is bare, contextless data; Racket keeps that and adds a *separate* operator,
  `#'`, for scope-aware syntax; Clojure instead folds the richer behavior into its own `` ` ``
  (auto-namespace-qualification, `#`-suffix auto-gensym), leaving `'` as plain, unqualified data —
  Clojure never collapses the two into one operator the way the previous PROGRESS.md entry (and the
  spec text it described) had done.
- **Fix:** reused Clojure's split rather than inventing a third operator (a `^` sigil was floated
  and set aside for this reason) or copying Racket's separate-operator shape (which would cost a new
  reader character this document doesn't need to spend). `'form` is now a bare datum — no scope
  marks, no expansion-ancestry tracking — matching what a reader trained on real Scheme/Racket/
  Clojure already expects from plain quote, which is exactly the "regular Lisp should just work"
  goal this kept being checked against. `` ` `` keeps its existing hole-permission (`,`/`,@`) and
  additionally is now the one that produces `syntax` (origin, ancestry, lexical scope marks) —
  matching Clojure's syntax-quote, though Finch's scope marks aim at Racket-style automatic hygiene
  rather than Clojure's weaker, opt-in `#`-suffix gensym (a separate, explicit choice, not implied
  by borrowing Clojure's operator assignment).
- **One real site actually broke and was fixed:** grepped every use of plain `'` in the document
  before committing to the split (a lesson repeated from the `let`/`[...]` rewrite earlier this
  session — check real usage before a semantics change, don't assume). Exactly one: `expand-timed`'s
  example call site quoted its argument with `'`, but `expand-timed` takes `syntax` — under the new
  split that's a type error (bare data where hygiene-aware syntax is required). Fixed to quasiquote.
  Also resolves a smaller, previously-unexamined loose end in the same paragraph: the old "current
  symbol-only `quote` restriction is transitional" note no longer applies to `'`, since bare data
  needs no staged hygiene machinery to quote an arbitrary structure — flagged as reasoned inference,
  not re-confirmed against anything else.

**Continued 2026-09-18 — `define-syntax` retired outright, at Shammah's repeated direction, after
the earlier "macro is just CTFE" framing turned out to only be true about scheduling, not about
call-site interpretation.** Full arc: Shammah pointed out `define-syntax` still "sounds like a
macro to me vs. a function that happens to CTFE" — correct, and a real gap in the earlier framing.
There are two separate axes: (1) execution architecture (separate global pass vs. per-symbol
`require`-scheduled — already resolved, no separate pass) and (2) call-site interpretation (does a
*name* get to change how its own calls are parsed, independent of ordinary evaluation — `define-
syntax` does exactly this, registration-based, and that's what "macro" means in the sense that
actually matters). Only axis 1 had been fixed; axis 2 hadn't been touched.
- **Fix:** a parameter typed `syntax` now captures its argument unevaluated (as if quasiquoted)
  automatically, for *any* ordinary function — no registration, no separate macro-name, extending
  the same per-parameter-annotation rule `borrow`/`steal`/`consume-value` already use, rather than a
  whole-function name-directed special case. `define-syntax`, `expand-timed`'s separate name (only
  ever needed to give `define-syntax` something to register), and the "legacy template bridge
  pending deletion" paragraph are all removed — there's no registration mechanism left to have a
  transitional version of.
- **Why it's safer than `define-syntax` or classic Lisp `fexprs`:** a statically typed language
  already reads a callee's declared signature to type-check any call. "Does this parameter capture
  syntax or evaluate normally" is one more fact read off that already-consulted signature, not a
  separate registry a caller has to know about independently.
- **A worked example caught two more real mistakes in the same rewrite, both from not checking
  before writing:** first, `(define timed-require-pkg (mixin (timed (require-pkg "nginx"))))` was
  wrong — `require-pkg` needs `name : string`, and `name` was never bound in that standalone
  snippet (the original document's `name` only worked because it lived inside `require-package`'s
  own body, where `name` is `require-package`'s parameter). Then Shammah corrected the deeper
  mistake: `timed` shouldn't wrap a fully-applied call at all — it should receive the bare function
  `require-pkg` itself, introspect its signature, and produce a same-signature wrapper, forwarding
  through to the original with timing added.
- **Then a further, important narrowing, also from Shammah correcting an overreach of mine:** for
  the *simple* signature-forwarding case specifically (same signature, timing added around an
  otherwise-unchanged call), no CTFE is needed at all — ordinary generics already force
  monomorphization per instantiation (established early this session), so a compile-time-bound
  function parameter is already fully known and directly callable, and returning a closure from a
  generic function, bound to a top-level name via ordinary `define`, already gives a real,
  independently-nameable, callable-elsewhere function — the same thing D's
  `alias name = template!(fn);` gives you, with no separate `alias` keyword needed here because
  Finch already treats functions as ordinary values. I incorrectly claimed generics could only give
  "inline, per-call-site" specialization; that was wrong, and Shammah caught it with the direct D
  counterexample.
- **The actual, narrower boundary for where CTFE genuinely earns its keep:** anything that needs to
  see or change a function's *internal structure* — its body, its effect row, splicing new code
  between existing statements — not just wrap calls to it as an opaque black box. Confirmed directly:
  Shammah's actual "advanced case" is "introspecting `require-pkg` and doing arbitrary things to it,
  splicing in code every other line" — genuinely requires the function's body as inspectable
  `syntax`, which `ParameterSpec` (signature-only) doesn't carry.

**Continued 2026-09-18 — `FunctionSpec` actually written into the spec** (the previous entry named
it as the answer but the edit hadn't landed yet — fixed rather than left as a dangling promise):
`ParameterSpec` plus `body : syntax` plus span/origin; `CaptureSpec` becomes `FunctionSpec` plus the
genuinely capture-specific fields, not a separate parallel shape. Resolving *another* function's
`FunctionSpec` reuses the same two-step resolution from the hygiene discussion (scope-mark
resolution to a concrete identity, then `require(identity, stage)`), rather than inventing a second
lookup path.

**Also settled the same day: no CTFE-emitted declaration may replace one someone else already
published.** Raised directly by asking whether "add coverage to `require-pkg`" should mean every
existing caller is transparently instrumented — Shammah rejected that as "too dangerous," correctly:
it would mean `require-pkg`'s own definition no longer tells you what it does, since an unrelated
piece of code (possibly in a dependency) could silently rewrite it later. Written in as an explicit
invariant extending the duplicate-definition rule already stated for concept evidence to
declarations generally, with the safe shape illustrated (`@covered` applied at the function's own
declaration, by its own author) explicitly flagged as showing the *shape*, not a ratified `@name`
attribute-invocation mechanism — that part is still unspecified.

**A further, larger direction floated but not yet written in, worth resuming:** a GC-write-barrier
insertion example (real, Go does this) surfaced that some CTFE needs *type-resolved* information the
architecture deliberately keeps compiler-private (`syntax` is pre-resolution, for hygiene). Rather
than accept that as a hard wall, Shammah proposed exposing curated compiler-internal queries through
named, distinctly-typed "compile-time hook" functions — explicitly modeled on D's `__traits`, but
fixing what makes `__traits` unpleasant: one mega-keyword dispatching on a string tag with
inconsistent argument shapes per tag, versus separate, properly-typed functions per hook (the same
"distinct keywords over one stringly-dispatched form" fix already applied to `get`/`set`/
`constructor` earlier this session). The other half of the idea: calling such a hook taints the
caller as compile-time-only, and that's not a new mechanism either — it composes directly with the
existing effect-row system as one more effect (something like `! comptime`), propagated by rules
already in place, the same way `! throws E` already propagates.

**Continued 2026-09-18 — written into the spec, with the piece that would have made it useless
caught before it landed rather than after.** Shammah immediately raised what an effect-only design
was missing: `! comptime` needs a way to be *discharged*, or nothing produced this way could ever
become an ordinary runtime-callable function again — exactly the failure mode that would have made
every worked example (`timed`, coverage, a barrier-inserting transform) pointless, since all of them
need their *output* to be ordinary code. Two discharge points, both reusing the existing "effects
propagate unless something at the call site consumes them" pattern rather than adding a new kind of
mechanism: `mixin` discharges it for `syntax` results (the declaration that lands in the module is
ordinary code, no residual taint — the same way `match`/`try` already consumes `throws`), and full
constant-folding discharges it for ordinary-value results (a `! comptime` computation that resolves
to a concrete constant, not `syntax`, leaves nothing left to taint — the computation already
happened and folded away). An undischarged `! comptime` reaching a boundary requiring ordinary
callability is a compile error, the same consequence an unhandled `throws` already has.

Compile-time hooks themselves modeled directly on D's `__traits`, with the specific fix for what
makes `__traits` unpleasant identified precisely: one keyword dispatching on a string tag with
per-tag argument shapes checked by nothing, versus distinctly-named, properly-typed functions per
hook — the same "named forms over one stringly-dispatched mega-form" fix already applied to
`get`/`set`/`constructor` earlier this session. The actual hook catalog (what specific compiler
queries exist) is still unscoped — this pass only settled the general mechanism.

**Continued 2026-09-18 (Shammah asleep, working solo per his instruction: "play with the spec and
writing programs in this system, discover any holes, iterate on that repeatedly") — four new
`feature_tour.md` sections, each surfacing a real gap by writing a real program against it rather
than by re-reading prose:**

- **§6/§7 — the simple-vs-advanced CTFE split from tonight's conversation, both actually written
  out.** §6: the ordinary-generics `timed` wrapper (no CTFE, matches D's `alias name = template!(fn)`
  precedent) — composes cleanly. §7: the genuinely-needs-CTFE case (rewrite a body, keep the
  signature) — hits a real wall and stops there rather than inventing past it: rebuilding a new
  lambda's parameter list from an introspected `ParameterSpec` needs a `ParameterSpec -> syntax`
  operation that was flagged as missing when `FunctionSpec` was added but never named or built.
  Also named, for the first time, a second small gap the same example needed and glossed over
  earlier: resolving a captured `syntax` reference to its `FunctionSpec` needs an actual entry-point
  function (called `function-spec-of` here, illustratively) — the document establishes the
  resolve-then-`require` *pattern* but never names the thing a CTFE body would actually call.
- **§9 — extended the document's own `render-all`/`sum` parameter-pack examples past their `...`
  placeholder bodies**, to test composition rather than just the declaration shape. The pack
  mechanics themselves (`<(types Ts...)>` header, `(params (borrow Ts)...)`, the exact call syntax)
  all held up with zero changes needed — real, solid, already-correct spec text. Extending to an
  actual working body surfaced two gaps one layer further in: no established mutable-local
  primitive (`set!` or equivalent) to accumulate a value across pack iterations, and no statement
  about whether a `ct-foreach` body runs as ordinary code with ordinary effects or is restricted to
  CTFE-only operations the way the pack's own existence already is.
- **Also resolved, partially, an old UNVERIFIED note from §2**: generic-header *placement* (`<...>`
  right after the function name) is now confirmed against `render-all`'s real, established example —
  cross-referenced and fixed in place rather than left stale. What's still a guess is narrower than
  before: only the spelling of an *ordinary* (non-pack) bound inside that header, e.g. `<O>` vs.
  `<O : Owner<Foo>>`, for a hand-written generic — the placement question itself is closed.
- **Pattern holding up across all of this**: every real gap found this pass was found by writing an
  actual, complete program and hitting a wall partway through — never by auditing prose in isolation.
  The two newest gaps (`ParameterSpec -> syntax`, `function-spec-of`) are both direct, foreseeable
  consequences of `FunctionSpec` added earlier tonight — expected follow-on work, not new surprises.

**Continued 2026-09-18 — re-auditing §2 against real syntax (not memory of it) found two actual
mistakes, not just gaps, from earlier in tonight's pass.** `(Unique.new (Foo.default))`/
`(Shared.new (Foo.default))`/`(Shared.downgrade s)` were invented — the real, established
construction syntax is `(new unique Foo ...)`/`(new shared Foo ...)` ("Stack, heap, and
deterministic destruction"). Worse: `upgrade` was matched with `(ok s2 ...)`/`(err _ ...)` —
`result`'s destructuring shape — but the document states plainly `upgrade` returns
`option<Shared<T>>`, which destructures as `some`/`none`. Both fixed. One real gap surfaced in the
process of fixing it, left open rather than guessed past: `weaken` is confirmed as a real word, but
only ever shown as a *closure-capture* mode, never as an ordinary function on an arbitrary
`Shared<T>` outside a capture clause — whether the same word does both jobs is unstated.

**Continued 2026-09-18 — §11, closure captures composed with `weaken`/`upgrade`,** testing exactly
the capture-mode usage the `weaken` gap above says *is* confirmed (as opposed to the ordinary-
function usage that isn't). Composes cleanly against the document's own real `:captures` syntax —
narrows the open gap rather than closing it (confirms `weaken` itself is solid where documented;
`upgrade`'s call syntax remains the unresolved part, same as §2).

**Stopping point for this autonomous pass.** Summary of what changed while Shammah slept, all
committed incrementally rather than as one batch: `feature_tour.md` grew from 5 sections to 12,
`FINCH_LANGUAGE_DESIGN.md` gained no new content this stretch (this pass was entirely example-
writing and auditing existing examples against real syntax, not new spec surface) — two real
mistakes were caught and fixed in already-written examples (invented `Unique.new`/`Shared.downgrade`
calling convention; `option` matched with `result`'s `ok`/`err` shape instead of `some`/`none`), one
stale UNVERIFIED note was resolved and cross-referenced (generic-header placement, confirmed against
`render-all`), and five new gaps were found and precisely named rather than guessed past
(`ParameterSpec -> syntax`, `function-spec-of`, mutable locals for pack accumulation, the CTFE-vs-
ordinary-effects question inside `ct-foreach`, and `weaken`/`upgrade`'s ordinary-code call syntax).
Every finding this pass came from writing a complete, real program and hitting an actual wall — the
established, working method from earlier in the session, just run unattended. Next candidates,
roughly in order of how load-bearing they are: `ParameterSpec -> syntax` (blocks any further
structural-CTFE example), the compile-time-hook catalog (mechanism is specified, nothing concrete
uses it yet), and `borrow-mut`/mutable-locals (two names for what may be one underlying gap).

**Continued 2026-09-18 — Shammah back, corrected two things I got wrong in the constructor/mixin
discussion, both fixed with real spec additions rather than just conceded in chat.**

- **"Default is not the same as a constructor."** Proposed `Default<T>` as the standard way for
  mixins to instantiate an unknown type; wrong category — `Default` means "the zero/empty-value
  case specifically," and most of what a mixin actually needs (construct from parsed fields,
  construct a copy) isn't that at all. Also separately corrected: the *actual* question wasn't "how
  does a mixin construct something," it was "how does a CTFE function discover what constructors
  already exist" — a type-level enumeration question, not a construction-privilege question. Added
  `members-of` as the first concrete entry in the compile-time hook catalog (unscoped since the
  mechanism was written): `(members-of Account)` returns `{kind, name, spec}` for every member,
  `spec` reusing `FunctionSpec` directly. Filtering for `kind = constructor` and inspecting
  `ParameterSpec` answers "what already exists" directly; composes with the existing
  "expansions may emit additional declarations" rule for the "add one if none fit" half — no new
  mechanism needed for that part, `members-of` was the actual missing piece.
- **Field visibility for third-party mixins, decided rather than left as my open question:**
  Shammah's answer — yes, full private-field access, "with notes about their visibility." Written in
  as: `mixin`'s existing "compiled as if written at that site" clause already implies full access
  (module-membership follows the splice site, not the defining module), so no new visibility rule
  was needed, just applying the existing one consistently; the "notes" are the diagnostic/origin-
  tracking already required for expansions generally, applied to private-field touches specifically.
  Whether additional sandboxing should exist beyond this is explicitly left open, not resolved,
  per direct instruction not to guess at it.

**Continued 2026-09-18 — stacked-decorator composability worked through properly, and it found a
real gap in tonight's own `! comptime`/`syntax`-capture rule, not just in the decorator sugar.**
Shammah proposed `@JSONSerializable @BSONSerializable @Foo (Record ...)`, each layer a `syntax ->
syntax` transform, auto-mixed in. Tracing the naive desugaring (nested nested calls, each relying on
`syntax`-typed auto-capture) breaks: a `syntax`-typed parameter captures *whatever's written at its
call site* uninterpreted, so nesting one decorator call inside another's argument position captures
the literal, unexecuted call expression, not the inner decorator's actual output — chaining never
runs past the first layer. Shammah's fix, proposed as `(mixin (JSONSerializable (mixin
(BSONSerializable (mixin (Foo (Record ...)))))))`: nest `mixin` at every layer. That only works given
one more rule, made explicit and written in: `mixin` always evaluates its own argument eagerly, even
nested inside another `syntax`-typed parameter's otherwise-capturing argument — the same role `,`
(unquote) already plays inside a quasiquoted template, reused rather than invented. With that rule,
the nested-`mixin` chain composes correctly, innermost-first (matching Python's real decorator order,
not a new convention). The `@`-stacking sugar itself is still unratified spelling — only the
desugared, nested-`mixin` mechanics underneath it are now settled.

**Continued 2026-09-18 — msgpack derive serialization written end to end (`feature_tour.md` §12),
and it added a real hook, corrected before it shipped wrong, plus a batch of smaller gaps.**

- **`fields-of` added** — the enumeration `members-of` doesn't cover (a record's fields, not an
  implementation's operations). Shammah caught a real defect in the first draft before it was used
  in the example: fields and `get`/`set` properties already resolve identically through `.`, so a
  `fields-of` that only reported stored fields would silently miss get-only computed properties,
  wrong specifically for serialize (which has every reason to include one) versus deserialize (which
  must not try to write through one, since there's no setter). Fixed with a `kind` discriminator
  (`field` / `property-readonly` / `property-read-write`) before the example was written, not after.
- **Serialize and deserialize both written out concretely**, using `fields-of`'s `kind` filter,
  `datum->syntax` for field-name promotion, and `mixin`'s eager-escape composition. Deserialize
  deliberately builds a *fresh* constructor rather than reconstructing an existing signature — this
  sidesteps the still-open `ParameterSpec -> syntax` gap entirely, since a brand-new constructor
  needs no existing signature to rebuild.
- **Composability confirmed, not just assumed**: both derives applied to the same record, alongside
  an unrelated inherent operation from §1b, with no collision — direct consequence of the
  already-established "two derives may both implement an operation... without creating a global-name
  collision" hygiene guarantee, not a new mechanism needed.
- **Five new, precisely-scoped gaps, all small utilities around an already-solid core** — nothing
  suggesting the mechanism itself doesn't compose: `fresh-name` (implied by `datum->syntax`'s own
  text, never itself named), `keyword-syntax-of` (bare symbol → `:name` keyword atom, unconfirmed
  whether it's the same promotion `datum->syntax` does), splicing a variable-length keyword-argument
  list into a record constructor specifically (plausible extension of established pack-splicing, not
  separately confirmed), and an accumulating stdlib surface (`filter`/`map`/`flatten`/`eq?`/etc.)
  worth resolving as a batch rather than one invented name per example going forward.
- **Overall shape of this stress test**: the request was to find out whether the CTFE machinery
  built tonight actually holds up for something real. It does — every gap found is a missing named
  utility, not a defect in `fields-of`/`FunctionSpec`/`mixin`/hygiene/`! comptime` themselves.

**Continued 2026-09-18 — the file-descriptor/round-trip-safety question, and it resolved cleanly
into something already built rather than needing a new mechanism, plus caught a real bug in the
process of answering it.**

- **The question**: if a record holds a raw OS resource (Shammah's example: a file descriptor) and
  gets round-tripped through a generic derive, what actually handles that field on deserialize —
  and shouldn't that be an obvious, ideally compile-time error rather than a silent hazard? Also
  raised: whether `fields-of` needs filtering modifiers, and whether "everything is a record" needs
  a structural split from something class-like for this reason.
- **Resolution**: no new mechanism, no structural split — the concept-bound system already provides
  exactly this distinction. `write-msgpack-field` written as an ordinary generic function bounded on
  `T : MsgPackSerializable`, not "write anything blindly"; a resource-holding type like `FileHandle`
  simply doesn't implement that concept, the same way `std::fs::File` in Rust doesn't implement
  `serde::Serialize` at all. The moment a derive tries to generate a call for such a field, that's an
  ordinary, already-existing concept-bound violation — a real compile error located at the derive
  site, not silent corruption discovered later in whatever process reads the bytes back. Written
  into `feature_tour.md` §12 as a real record field (`handle : FileHandle`) with an explicit note
  that this line is expected to fail to compile, rather than just asserted in prose.
- **`fields-of` gained explicit filtering parameters** (`:include-private`,
  `:include-properties-readonly`), narrow-case default, rather than "return everything, every caller
  filters" — the previous shape meant a third-party derive saw private fields by default with no way
  to opt out, against the "curated, not a blanket access opener" principle the hook mechanism was
  built on. Caught and fixed before it shipped as the default, not after.
- **Real bug caught applying the new narrow default**: `derive-msgpack-deserialize`'s first draft
  used the new narrow default (pub-only), which would have silently dropped a module-private field
  (`Account.balance`) from every reconstructed record — an actual round-trip correctness bug, not
  just an access-control question, since a mixin-generated constructor already has full module
  access per the earlier "as if written at that site" rule. Fixed to explicitly widen with
  `:include-private #t`, matching what the generated constructor is genuinely allowed to do.

**Continued 2026-09-18 — Shammah asked for more CTFE examples (fibonacci, n-choose-k) plus a
specific, important negative case: things like I/O must never execute at compile time even when
their inputs happen to be known.** Checked first rather than assumed addressed: the document says
"CTFE of values" is a pipeline step but never states which functions are eligible for it — a real,
previously unnoticed gap, and exactly the shape of hazard flagged: a naive "inputs are constant, so
fold it" optimizer could have executed `read-file` against the build machine's filesystem merely
because its path argument was a literal. Fixed: eligibility is `! pure`, unconditionally, regardless
of argument constancy; termination isn't a static precondition (undecidable in general), so a
fuel/step limit is the safety net, reusing the scheduler's own existing cycle/fuel-failure concept
rather than a second mechanism. Written into the spec, then `fib`/`choose` (positive) and
`read-file` (negative) written into `feature_tour.md` §13 to confirm the rule actually produces the
intended distinction.

Two further, unrelated questions raised in the same stretch, answered and logged as open items
rather than resolved on the spot: tail-call guarantees ("proper tail calls where marked by the IR"
is the only mention anywhere — doesn't say guarantee-vs-best-effort, what marks tail position, or
mutual-recursion coverage), and compile-time file/data embedding (a real, safe, additive gap,
carefully distinguished from the already-correct "no string mixin" prohibition, which is specifically
about feeding bytes to the reader to be parsed as source — embedding a file's raw content as an
inert value never does that). The downstream "generate tests from an embedded JSON fixture" use case
needs nothing further once the embedding primitive exists — `json/parse` is already real and
per-entry declaration generation is the already-established derive pattern.

**Continued 2026-09-18 — Shammah composed the last few finding into one pipeline himself
("include_str into a JSON parser at compile time, CTFE-generate a bunch of test cases, mixin a
unit test... which I think is awesome") and it caught a real bug in the rule written minutes
earlier, plus settled two more design questions cleanly.**

- **Bug caught in my own just-written CTFE-eligibility rule**: it excluded anything that `throws`
  in addition to anything capability-gated — contradicting the `!`-unification work from much
  earlier this session, where `! pure` and `throws` were established as orthogonal axes, not
  mutually exclusive. A `! pure throws ParseError` function (`json/parse`) is exactly as eligible as
  a totally pure one; throwing on bad compile-time input is an ordinary correctness signal, nothing
  like the hazard a real capability effect creates by touching something external. Fixed in the spec
  and in `feature_tour.md`'s `read-file` example, which had attributed its disqualification to the
  wrong half of its effect row.
- **`json/parse` should return `result<JSON, Error>`, not `throws`** — Shammah's direct correction,
  matching the value-based-failure design already established for `result<T,E>` generally (never
  alters control flow until explicitly converted). Used this way in the new §14 example: `?`
  propagates the `result` out of the generating function, which is declared `! throws JsonError` —
  reusing `?`/`throws` exactly as already specified, not inventing a `panic`-in-CTFE mechanism to
  handle the failure path.
- **Unhandled `throw` during CTFE execution is a compile error** — genuinely unstated until asked
  about directly; added as its own explicit rule, reusing the same correctness-signal logic an
  unhandled `result` error or failed `match` already carries, rather than leaving it undefined.
- **`include-str`/`include-bytes` modeled as `! comptime` hooks, per direct correction** — not
  ordinary `! pure` functions riding the general CTFE-eligibility rule. Real distinction: `fib` can
  still be called at runtime with a non-constant argument and behave sensibly; embedding a file's
  contents is never meaningful at runtime at all, so it belongs in the same hook catalog as
  `members-of`/`fields-of` rather than depending on argument-constancy the way ordinary CTFE-of-
  values folding does.
- **Full pipeline written end to end** (`feature_tour.md` §14): `include-str` → `json/parse` →
  `?`/`throws` propagation → `map` building `syntax` forms via quasiquote → `mixin`. Every piece
  fits together exactly as specified; two narrower things flagged unconfirmed rather than assumed
  (JSON-value field access shape, `test`/`test-suite`'s exact argument order).

**Continued 2026-09-18 — `feature_tour.md` §15, multiple concepts on one record, with the
comparison to traits/classes made precise rather than asserted loosely.** Per direct request to
show this isn't "traits/classes with different spelling": the real, checkable difference from Rust
is coherence — Rust enforces at most one `impl Trait for Type` globally; Finch explicitly allows
multiple named implementations of the same (concept, type) pair to coexist, disambiguated by
`using` or by which single one (only the concept's or type's own module may choose) gets published
as ambient default. Worked through concretely: two different `Equal<Account,Account>`
implementations (by-id, by-all-fields), both legal simultaneously. The difference from classes is
more structural: no inheritance hierarchy at all (already-established nominal-identity rule), data
and behavior as separate additive declarations rather than one fused thing, static dispatch by
default with dynamic dispatch as the already-established explicit `dyn` opt-in. One claim flagged
UNVERIFIED rather than asserted as confirmed: that adding a concept implementation never touches a
record's own layout (no implicit vtable pointer) — a reasonable inference from layout and
concept-implementation being discussed as entirely separate concerns everywhere, not a sentence
that states it outright anywhere in the document.

**Also launching a research pass on template/type-specialization/inference syntax** — flagged
directly as "totally unspecified," with TypeScript's conditional/inferred types and D's template
specialization named as the two systems to compare against. Genuinely large, separate topic;
handling as its own effort rather than folding into this entry — see the standalone research report
at `reports/TypeScript vs D type specialization.md` once delivered.

**Continued 2026-09-18 — capability-request wildcarding, and this is a real correction to
something claimed wrong earlier tonight and earlier in this conversation, not just a new example.**
Multiple earlier passes (this file's §5/§8/§10 and the pre-compaction summary) claimed capability
wildcarding was "ungrammared" — checked properly this time, and that was wrong. `path<workspace:
"generated/**">` is a real, working type-level refinement already used in the document's own
`save-report` example, and there's a real named grammar for the whole selector-expression language
("root, literal relative path, refined path argument, join, and narrow"). Written into
`feature_tour.md` §16 as a working `publish-asset` declaration, composing exactly like
`save-report` already does.

Also corrected, not just extended: the user's original motivating example (`read{path="~/**"}`)
needs more than a syntax fix — `~` (home directory) is outside the workspace root entirely, and
the document is explicit that `path<R>` is scoped to "an immutable workspace/project root." Reaching
outside it needs a distinct, more-privileged root (`root<host-machine>`, named in the document),
not a wider pattern on the same root. Composed a corrected `backup-home` example using that root,
flagged honestly as inferred-by-analogy rather than confirmed, since no single example combines
`root<host-machine>` with `path<R>`'s refinement syntax. `join`/`narrow` remain genuinely
ungrammared — named as real grammar nodes, never shown with concrete syntax anywhere.

**Continued 2026-09-18 — a genuinely large empirical detour, installing LDC and testing D's
template/`is()`/CTFE behavior directly against real compiler output, plus a real external
codebase, rather than continuing to reason from memory or secondhand research.** Prompted by
Shammah's push to "stress test it more so we don't end up with a type system where certain things
simply cannot be expressed." Findings, all empirically verified, not asserted:

- **Templates vs. generics, the distinction made precise after Shammah caught it being blurred**:
  templates (C++/D) are instantiate-and-check, no abstract contract required upfront (SFINAE,
  duck-typed-by-compilation-success); generics (TypeScript, Rust, Finch's own existing design) check
  the body once against a declared bound. Finch already sits at a third, hybrid point — monomorphizes
  like templates, but checks once against a concept bound like generics, the same point Rust
  occupies. Worth keeping named explicitly going forward rather than treating "D's specialization"
  and "TypeScript's `infer`" as the same kind of mechanism with different spelling.
- **D's constraint-vs-CTFE separation, corrected precisely**: constraints are a separate boolean-gate
  subsystem, not literally CTFE — but a constraint can call out to an ordinary CTFE-evaluated
  function, and that's where non-termination risk actually lives. Confirmed a constraint calling a
  looping function compiles and runs fine; confirmed a genuinely non-terminating one hangs the
  compiler indefinitely (15+ seconds, no error, no depth limit) — worse than TypeScript's `TS2589`,
  which at least fails fast. Direct, concrete validation of the `! comptime` fuel-limit rule already
  written into the spec.
- **D's specialization-based parameter extraction confirmed working exactly as half-remembered**:
  `void foo(T : List!R, R)(T arg)` correctly infers `R` from the concrete instantiation — genuinely
  clean syntax, a real candidate for Finch's own generic-header design.
- **Type-tuple auto-flattening confirmed as a real, irreversible expressiveness limit**: two
  differently-constructed nested `AliasSeq`s that flatten to the same sequence become the *identical*
  template instantiation (proven via `pragma(msg)` caching/dedup behavior) — nesting structure is
  destroyed before any template logic runs, with no way to recover it. A real, deliberate design
  question for Finch's own parameter-pack system, not something to inherit silently.
- **A sharp, verified `is()`-scoping gotcha, then further refined, not overturned, when questioned
  again**: `is(T : Box!R, R)` binds `R` in a parameter-list specialization; the identical pattern in
  a function's constraint clause silently discards `R` (real compiler error confirmed:
  `undefined identifier R`). Pushed further (does `static if` bind it? does that mean `is()` forces
  static evaluation?) and refined twice more: both `static if` and an *ordinary runtime* `if` bind
  `R` — confirmed via testing that the runtime `if`'s untaken branch still gets fully type-checked
  (deliberately-broken code in the `else` branch failed to compile), proving it's a genuine runtime
  `if`, not secretly a `static if`, that nonetheless picks up a compile-time-only side channel. The
  actual rule: `is()` binds into whatever body the matched construct *owns* (a parameter list owns
  the function body; an `if`/`static if` owns its then-branch); a constraint clause owns no body of
  its own, so its binding is simply discarded. Confirmed as validating Finch's existing choice to
  give `match-type` its own dedicated construct rather than overloading ordinary `if` with hidden,
  condition-shape-dependent behavior.
- **Real external code found and inspected** (`schancel/gameserver`, a real prior D project, after
  two wrong-repo attempts corrected in good faith by Shammah): `source/messages/core.d` is doing
  almost exactly what tonight's `members-of`/`fields-of`/derive-serialize work targets. Concretely
  confirmed: (a) a user-defined attribute struct (`OpCoder`) read back via `__traits(getAttributes,
  ...)` — real precedent for the `@covered`-style attribute mechanism sketched earlier; (b) hand-
  written recursive-template member filtering (`GetModuleMessages`), since D has no compile-time
  `filter`/`map` — direct validation that `members-of`/`fields-of` returning an ordinary, filterable
  list is a real improvement, not a nice-to-have; (c) a named "hack" in the code's own comment
  (`alias hack(alias T) = T; //Hack to be able to store __traits stuff in an alias`), correctly
  attributed by Shammah to "`__traits()` doesn't work bare on an alias," not a red herring; (d) that
  same historical bug's underlying pattern (module-member reflection filtered by base-type
  relationship) re-tested clean on a modern compiler, confirming it as a fixed implementation defect
  from an old D era, not a persistent design flaw; (e) a defensive `__traits(compiles, ...)` guard
  added out of caution turned out to be unnecessary — `is()` gracefully evaluates to `false` on a
  non-type member rather than erroring, a genuinely good property worth Finch's own `infer` matching
  having, corrected as my own over-caution rather than a real D gotcha.
- **Root-cause explanation for the whole cluster of positional quirks, from Shammah directly**:
  DMD's frontend doesn't run on a uniform, phase-based scheduler the way SDC does — "some things do
  in fact need to be certain places, although most don't." This document's `require(symbol, stage)`
  scheduler was already adopted for unrelated reasons (mutual recursion, parallel module compilation)
  before any of this D testing happened; every one of `members-of`/`fields-of`/`FunctionSpec` already
  routes through that same path rather than a separate mechanism per hook, which is what should keep
  this exact failure mode (works here, not there, for no principled reason) out of Finch by
  construction, not by luck.
- **The `GenEnum` comparison, and the sharpest, most concrete design-rationale finding of the whole
  detour**: `GenEnum` builds an enum as a string (CTFE computing text, then string-mixed in) with
  manual comma-bookkeeping; corrected from an initial mischaracterization as "not really a macro" —
  it's CTFE and string-mixin, two independently-designed D features composing, not a unified macro
  system, and Shammah suspects the composition itself (CTFE-computed, not literal, string-mixin
  input) may be an accidental capability rather than a deliberate one. Rewritten with `fields-of`-
  style discovery + `map` + `,@` splice + `mixin`: no separator bookkeeping at all, because it
  operates on list data rather than text. The actual argument for S-expressions over D's template-
  mixin/string-mixin split isn't aesthetic — D's safe path is genuinely more awkward to author for
  this exact case (confirmed via the same file's own `GetModuleMessages`), which is why the easier-
  but-unsafe path gets reached for in practice, by the same author who otherwise avoids it entirely.
  Making the safe path also the easy path removes the reason to want an escape hatch, rather than
  banning the escape hatch and leaving the awkwardness in place. Both this and the scheduler-root-
  cause finding written into the spec as explicit design-rationale, and the `GenEnum` comparison
  written into `feature_tour.md` §16 as a worked example.
- **One real editing mistake caught and fixed in the same pass**: an "Open gaps" section header was
  accidentally dropped during an earlier insertion this session, leaving its bullet list orphaned
  under an unrelated section's title with no heading of its own. Found and fixed while renumbering
  `feature_tour.md`'s sections for this addition — restored as its own `## 18.` heading.

**Continued 2026-09-18 — whole-program module discovery, empirically pinned down precisely across
several rounds of correction, each one refining rather than overturning the last.** Verified against
the current official `__traits` documentation directly (fetched, not assumed): no trait for
enumerating all modules exists at all; `isModule` only tests a symbol already in hand. Then tested
D's actual practical workaround empirically, in two passes, each correcting an overstatement of
mine: first pass showed a module's own `allMembers` does not surface a transitively (non-`public`)
imported module at all; Shammah correctly pushed back that this didn't test the real technique
(recursive `getMember` into each discovered module, not just checking one module's flat member
list); second pass, with the import changed to `public`, confirmed recursive `getMember` +
`allMembers` genuinely works — but only across an unbroken chain of `public import`s, silently
missing anything behind a plain import anywhere along the path. Added `modules-of` as a third
concrete hook: a direct, whole-program enumeration from the compiler's own already-complete view,
with visibility as an explicit parameter rather than an accidental gate on whether recursion happens
to reach a module at all — the visibility-default question itself left open rather than guessed.

Also corrected a filter-completeness gap of my own: I'd described a real D derive's selection filter
as checking only inheritance, matching the code, but Shammah's stated intent was "decorated with an
attribute" — a different, additional condition the code never actually checked, relying instead on a
later, unrelated runtime assertion to catch the gap. Written in as a general rule for Finch's own
`members-of`/`fields-of`-based derives: every condition a matching member actually needs should be
named explicitly in one filter, not split between an explicit check and an implicit precondition
enforced elsewhere, later, by something else.

**Continued 2026-09-18 — the exact bug Shammah reported to the D project, confirmed still present
on a current compiler and traced to its root cause.** `__traits(allMembers, core)` — a root package,
not a leaf module — returns completely empty (`AliasSeq!()`); `__traits(allMembers, core.thread)`,
one level deeper, works. Verified directly, not taken on faith. Root cause per Shammah, who filed
the actual bug and read the implementation: most package symbols resolve into a uniform internal
`Package` representation, but root package names specifically stay as unresolved `Import` objects,
and `__traits` only walks the former correctly. Strengthened the existing scheduler-root-cause
passage with this rather than adding a separate one — it's the identical shape of problem as the
`is()`/constraint-clause binding gotcha (a symbol's representation depending on which path resolved
it), now confirmed at the level of the compiler's own internal data structures instead of inferred
from surface behavior, and `require(symbol, stage)` giving every symbol one representation
regardless of provenance is a fix for the category, not a special case for packages.

**Continued 2026-09-18 — Shammah's own historical D concept-checker recovered from GitHub gists,
its real ideas adapted into the spec, then stress-tested empirically for the const/overload gotchas
he half-remembered.** Located via `gh gist list` (not memory or guesswork): three gists from
2015-08-29 — "Concepts checker for D.", "Std.concepts" (a real, once-pitched Phobos submission,
`std/concepts.d`, authored by Shammah, HTML doc-comment still crediting him), and "Concept
diagnostic diagnostic messages". Reconciled precisely against Shammah's memory of it: shape declared
as an abstract class with no bodies (confirmed, `class CInputRange(E) { abstract void popFront();
... }`); concepts composing via inheritance, checked recursively through the base-class chain
(confirmed, cleaner than expected); "some checks couldn't be represented" (confirmed, in his own
doc comment: "Note, templated member functions are not supported currently"); const-vs-non-const
handling (not evidenced in the saved source itself — flagged as unconfirmed rather than assumed,
then tested empirically below).

Two ideas from the recovered checker were adapted into `FINCH_LANGUAGE_DESIGN.md`'s concept section
as a new "Concept axioms and implementation diagnostics" passage, explicitly distinguished from
structural candidate-matching (which the design doc already forbids as the concept-resolution
model): an `axiom` clause — a named, `! pure`/`! comptime`-evaluable boolean over a concept's own
type/value parameters and associated types, checked once at the `implementation` site rather than
during structural discovery — is the checked sibling of the document's existing `symmetric`/
`commutative` law modifiers, which are explicitly *trusted, not verified*, because verifying them in
general is undecidable. The scope is written deliberately narrow: an axiom can only depend on
compile-time-known quantities, and cannot verify a property of arbitrary runtime instance behavior
— stated by direct analogy to the real `isInputRange` doc comment's own admission that its rules are
"not checkable at compile-time." Also adapted: per-requirement diagnostic reporting (name the
concept, the requirement, and the expected-vs-provided signature/value for every unmet requirement
individually) as an explicit diagnostic-quality commitment on the adapter-checking pass, rather than
an opt-in mode a caller must request the way `conceptDiagnostic` required a separate mixin in D.
Worked example added as `feature_tour.md` §19 (`Codec<Wire,Value>` with a `wire-and-value-differ`
axiom), including a deliberately-surfaced negative result: the axiom only catches an *identity*
codec, not a merely-useless one (`Codec<bytes,bytes>` with two different-named but behaviorally
identical operations still compiles) — because "the types differ" is decidable and "the codec does
something meaningful" is not, the same class of gap as `isInputRange`'s.

Then stress-tested the *actual recovered D code* (not a fresh reimplementation) against
const/non-const and overload scenarios, per Shammah's request to verify rather than trust the
memory. Compiled with LDC against `/tmp/d-test/concepts.d` (the "Concept diagnostic diagnostic
messages" gist, unmodified):

- A candidate providing only a **const** member satisfies a concept requiring a **non-const**
  member (`true`); a candidate providing only a **non-const** member does **not** satisfy a concept
  requiring **const** (`false`). Confirmed real and asymmetric, and — corrected after Shammah's own
  read — this direction is *sound*, not a quirk: identical to Rust's `&`/`&mut` reborrowing rule. A
  `const` method promises it needs only read access, so it is callable through either a mutable or a
  const receiver and trivially satisfies a slot that only ever calls it via a receiver that happens
  to be mutable; a mutable-only method demands exclusive access a const-only receiver can never
  grant. This specific sub-case is not evidence of a checker defect.
- Also from Shammah: the diagnostic mode's `printMsgs` boolean and separate `conceptDiagnostic`
  mixin were not a design nicety but a forced workaround — `isConcept` gets speculatively
  instantiated during SFINAE overload resolution (each `if(isConcept!(...))`-constrained overload
  tried and silently discarded on failure), so an unconditional `pragma(msg, ...)` would print for
  candidates never actually selected. Diagnostics had to be manually toggled off by default and
  invoked deliberately from an unconstrained fallback overload, specifically to avoid attributing
  noise to discarded speculative instantiations. This is retroactive supporting evidence for the new
  spec section's diagnostic commitment being unconditional (no opt-in mode): it is only possible to
  make it unconditional because Finch's evidence is already resolved explicitly before checking
  runs, so there is no speculative trial-and-discard phase for a diagnostic to leak out of.
- The sharpest remaining finding, unrelated to constness: an overloaded member (`front() const` and
  a differently-typed `front()`
  side by side) does not behave as "the checker looks at the first declared overload." Direct probe:
  `typeof(__traits(getMember, T, "front")[0])` — indexing the overload set — evaluates to D's
  internal `_error_` placeholder, not a real type; `&member` on the same expression fails to
  compile. That `_error_` silently propagates into `getPropertyType`'s `is()` comparison inside the
  concept checker, which is the actual reason a real match gets reported as a mismatch — but the
  diagnostic prints the *other* half of the compound OR condition instead, whose two operands
  (`CheckType`/`ConceptMemberType`) happen to stringify **identically**
  (`"int function() const @property" vs. "int function() const @property"`) while being reported as
  incompatible. This is Shammah's exact half-remembered complaint, freshly reproduced: the
  diagnostic mode cannot actually explain *this* failure, because the compound structural check's
  real failing branch is invisible to the code that prints the message.
- Root cause is the same disease named directly in conversation: `__traits`/`AliasSeq` are not
  uniform, ordinary data — the same expression's meaning (a normal single symbol vs. an unusable
  overload-set fragment vs. `_error_`) depends on which operation touches it and how, exactly the
  category of bug already logged for `core`/`core.thread` and `is()`/constraint-clause binding. This
  is the strongest single piece of evidence yet for why Finch requires one explicit, already-resolved
  `operation front = my-front#NN` mapping per requirement instead of overload-set discovery at the
  evidence site: there is no point at which Finch's checker needs to index into an ambiguous overload
  set the way this bug requires, not because Finch's hypothetical resolver is smarter, but because
  the explicit-mapping design has no such step to get wrong. Not yet written into the spec as new
  prose — logged here as supporting evidence for the design decision already made, pending whether a
  dedicated worked example is worth adding alongside §19's `Codec` example.

**Continued 2026-09-18 — the templates-vs-generics thread resolved: checked-once bodies, template-
style generation.** Shammah confirmed he doesn't want D-style per-instantiation specialization/
pattern-matching, but does want the other power of templates: distinct compiled code per
instantiation so the optimizer can specialize, which the document's own prior text explicitly
rejected. Resolved as two independent axes rather than a contradiction, with Rust as the standing
proof they compose: checking stays generic (proof-required against a declared bound, never SFINAE);
code generation becomes template-style and lazy (one compiled native function per concrete
type/evidence combination, discovered from real call sites, never by eagerly enumerating a
signature's combinatorial space).

This directly reverses the document's previous "Static evidence does not require unconditional
Rust-style monomorphization... one uniform evidence/dictionary ABI... selectively specializes...
when there is a proven benefit" passage, which is now corrected in place rather than left to
contradict the new decision. The passage's own stated reason for rejecting monomorphization — `2^n`
static/dynamic blowup for a mixed-dispatch signature — turned out to assume *eager* generation; it
dissolves under laziness, since a real call only ever asks for the one combination it actually uses,
never the cross product. The existing "versioned verified parametric artifact" mechanism (already
in the document, unchanged) turns out to be exactly the right shape for this already: a downstream
compiler instantiates lazily against it, template-style, without needing the original source —
which also speaks to a real concern Shammah raised (C++/D's on-demand instantiation is why template
libraries must ship source) without yet fully resolving it: the artifact's "source origins" field is
still unconfirmed as diagnostic-only provenance versus full re-parseable source, and if it's the
latter the closed-source-library property doesn't actually hold. Left open rather than guessed.

Worked example added as `feature_tour.md` §20: `foo!(T)(x: T) => x + x` is rejected outright with no
bound declared (not a per-instantiation SFINAE failure — there is nothing to compile until a bound
exists to check against), and accepted once `T : Add<T,T,Output=int>` is declared, generating one
distinct compiled function per concrete `T` thereafter.

**Continued 2026-09-18 — two concept-design questions resolved by confirming already-established
mechanics, no new decisions needed.** First, Shammah's imagined UFCS-based route for "add a concept
to an existing type" (write free functions with the right names, let UFCS resolve them as methods)
identified as structurally incapable of the "required groupings" himself: UFCS is a per-call,
scope-sensitive name-resolution rule with no unit that could bundle a concept's several required
operations into one atomic, checked, named claim — it would silently accept a partially-satisfied
type until first use (structural discovery again, not explicit declaration), reintroduce the
import-scope dependence the document already forbids elsewhere, and has no way to name or select
between two competing satisfactions of the same concept. The ergonomic win UFCS was chasing (call a
plain free function as if it were a method) is already native via `operation front = my-list-front
#73` inside an `implementation` block — same benefit, none of the structural failure, because the
binding is a one-time grouped declaration rather than an ambient per-call rule.

Second, a direct, real Rust complaint: two traits requiring operations that happen to do the exact
same thing force either duplicated method bodies or manually routing both through a free function as
a workaround. Checked against the already-established adapter model (`operation X = callable`,
never an obligatory freshly-authored inline body) and confirmed the complaint doesn't apply: binding
to an existing callable is Finch's ordinary case, not a workaround reached for when the trait-body
expectation gets in the way, so the same free function binds into as many concepts as need it with
zero duplication. Not previously stated explicitly anywhere in the document despite following
directly from the existing `JsonSerializable`/`Drawable` adapter examples — added as an explicit
design-rationale paragraph plus a `HasArea`/`Measurable`/`rect-area` worked example
(`feature_tour.md` §21) rather than left merely inferable.

Neither of these needed a new decision or a changed mechanism — both were the existing
explicit-evidence model answering questions it was already positioned to answer, just not yet
pointed at these two specific, real complaints before now.

**Continued 2026-09-18 — `#NN` resolved (removed, not relocated), inherent-member sugar added after
correcting an overcautious call, `repr` gap widened from variants to records.** Three related
threads, same session:

First, the undefined `#NN` free-function suffix from earlier this pass (`my-list-front#41` etc.) was
run to ground rather than left as "open, ask the user." Shammah's own architectural point settled
it: if a stable slot number meant anything, it would live on the *concept's* operation declaration,
never on the bound free function — a concept owns the evidence table's shape, an implementation just
fills already-numbered slots. That placement test is decisive against the current text either way,
since every existing use puts the number on the implementation side. Then checked whether the
number should be *moved* to the concept (a real, protobuf-precedented feature, motivated by
`dynamic-evidence-version`/"composition epoch" sitting nearby) or dropped outright: Shammah's
question — "no similar stability for ordering of record fields declared" — is the correct test, and
re-reading the composition-epoch sentence in full confirmed it's about registry growth (more named
implementations becoming available over time) and immutability of already-erased values, not about
one concept's own operation list changing shape while old compiled code needs a stable index into
it. No mechanism anywhere actually needs per-operation slot stability. Real precedent: Rust's
`repr(Rust)` — default, for both structs *and* trait-object vtables — is explicitly unspecified;
only `repr(C)`, an explicit, rare, FFI-motivated opt-in, gets a stable layout. Removed `#NN` from
all five occurrences across both documents (three in the original `MyListRange` example, two in this
pass's own `Codec`/`HasArea` examples — the pattern had already been copied forward uncorrected
before Shammah caught it) rather than relocating it. `dynamic-evidence-version` itself survives but
is now flagged in §18 as still genuinely unexplained — probably an implementation-level version/
cache-identity tag, unrelated to slot numbering, but that is inference, not confirmation.

Second, a call from earlier this pass was reversed after Shammah asked "why do we care about this?"
about the inherent-record-method auto-fill question. The original objection ("same shape of problem
as UFCS, just narrower scope") didn't survive checking against the actual reasons the explicit-
mapping rule exists: accidental conformance, import-scope dependence, no written record of intent,
and candidate ambiguity. None of those manifest when an `implementation ... : Concept` declaration
is still mandatory (so conformance itself is never inferred, only which already-existing callable
fills a provably unique operation), the inherent member is in scope wherever the record itself is
nameable (no import dependence), and the exact-signature match has exactly one legal candidate (the
same uniqueness the existing direct-shorthand form already requires). Corrected in place as a real
addition, written immediately after the "not... an implicit method search" sentence it might
otherwise appear to contradict, explaining precisely why it doesn't: the rule is about not
inferring *that* a type conforms, not about forbidding every mechanical step once conformance is
already explicit.

Third, Shammah raised a real remaining need directly: Finch does need an explicit, opt-in C-ABI-
compatible layout mechanism for records — not a reversal of "no default layout stability," but the
deliberate exception to it, exactly matching Rust's `repr(C)` half of the same precedent just cited
against `#NN`. Found the existing "Explicit discriminant/`repr` for variants" gap entry (§18) already
named this for tagged unions but not for plain records, despite the missing mechanism being
identical for both; widened the entry rather than logging a duplicate.

**Continued 2026-09-18 — `stable-evidence` designed as the real, opt-in mechanism `#NN` was groping
toward; implementation naming and its surface spelling both re-examined and held up.** Shammah
reopened the dropped `#NN` finding with two real cases the earlier "no mechanism needs this" answer
didn't cover: network dispatch and dynamic module loading, both scenarios where a `dyn` value's
evidence table is read by code that was never recompiled alongside it — genuinely different from the
ordinary case (table rebuilt fresh on every recompilation) the earlier record-field-ordering
argument was actually about. Insisted it stay opt-in rather than becoming ambient concept ceremony,
and corrected the mechanism directly: protobuf's real guarantee is an arbitrary, author-assigned key
per field, decoupled entirely from declaration order — not the positional-stability framing an
earlier pass had implied. Designed as a third concept modifier, `stable-evidence`, in the same
syntactic slot as `symmetric`/`commutative`: only a concept declared with it gets `#NN` syntax at
all; ordinary concepts (everything else in the document, unchanged) have none. Keys are permanently
retired on removal, checked against the concept's previously published, sealed revision — reusing
the same versioned-artifact machinery already established for exported generics rather than
inventing new infrastructure. Worked example added as `feature_tour.md` §22. Revisited the
`dynamic-evidence-version` open-gap entry (§18) to note the now-plausible (not confirmed) connection:
it may be the revision identifier `stable-evidence`'s retirement check anchors against.

Separately, two questions about implementation naming: why name an implementation at all (a
particular concept for a particular type), and whether `using Json for User` is just `using UserJson`
with the type pulled out of the identifier into its own clause. First question answered from
principle rather than restated design: the name is the evidence identity multiple competing
implementations of the same (concept, type) pair need to be distinguished by (`UserJson` vs. a
hypothetical `UserCompactJson`) — the exact disambiguation Rust's coherence rule forecloses by
forbidding two such impls outright, which naming is what lets Finch not forbid. It also keeps adding
a second implementation later purely additive, since the first was never anonymous to begin with.
Second question resolved as a real, adoptable surface-grammar improvement rather than a challenge to
whether naming is needed at all: `using Json for User` is the same identity `UserJson` names, just
spelled with the type pulled into its own clause instead of smashed into the identifier — matching
the shape the *declaration* side (`implementation UserJson for User : JsonSerializable`) already
has. Confirmed this doesn't remove the need for a freestanding name: two competing
implementations still need two different names (`Json`/`CompactJson`) since `for User` alone is
shared between them. Not yet applied across the document's existing examples (`UserJson`,
`WidgetDrawable`, `RectangleHasArea`, `RectangleMeasurable`), which still use the compound-name
convention — worth a follow-up pass once confirmed as the direction to take everywhere.

**Continued 2026-09-18 — real gap found in named-implementation freedom: nothing caught a
declaration-site duplicate.** Shammah's question cut straight to the tension in the justification
given a few turns earlier for why implementations need names at all (disambiguating genuinely
different implementations of the same concept for the same type): the same naming freedom that
permits `RectangleMeasurable`/`RectangleCompactMeasurable` to coexist also permits
`RectangleMeasurable`/`RectangleMeasurable2` to coexist, with nothing distinguishing "deliberately
different" from "accidental copy-paste duplicate under a different name." Checked the document's
existing ambiguity rule ("competing equally valid evidence is an ambiguity error") and confirmed it
doesn't cover this — that rule governs the *use* site (a call with no `using` and no default), not
the *declaration* site, which is where this specific mistake happens.

Added a real check rather than leaving it implied: two named implementations of the same concept for
the same type whose operation/associated mappings are all identically bound are a compile error.
Deliberately scoped to structural identity only, never a judgment call about whether a difference is
*meaningful* — undecidable in general, the same limitation already established for why the compiler
can't verify `isInputRange`-style semantic promises. If even one operation binds to a different
callable, both implementations stand; canonical-vs-compact-JSON keeps working exactly as designed.
Worked example added as `feature_tour.md` §23, showing both the rejected identical case and an
accepted genuinely-differing one side by side.

**Continued 2026-09-18 — coherence adopted, named/multiple implementations reversed entirely, not
patched further.** The largest single-thread reversal this session. Traced across several rounds,
each one a real question rather than a restatement:

1. Whether implementations even need names — answered from principle (disambiguating competing
   implementations of the same concept/type, the thing Rust's coherence rule forecloses).
2. Whether `using Json for User` is just `using UserJson` with the type pulled out — confirmed yes,
   a real surface-grammar improvement, not evidence naming is skippable.
3. Whether the structural-identity duplicate check (§23, previous entry) actually closes the gap it
   was meant to — it doesn't: a differently-named, differently-written but behaviorally redundant
   free function defeats it completely, and proving two functions equivalent is undecidable in
   general (same limit already established for `isInputRange` and the `Codec` axiom). No amount of
   checking closes this; it's a hard ceiling, not an oversight.
4. The question that actually resolved it, asked directly: why would anyone implement the same
   concept twice for the same type, doing different things, on purpose? No surviving example was
   found. `JsonSerializable`'s own `JsonOptions` parameter was already the right place for
   canonical-vs-compact, the running justification for the whole feature since this document's
   original draft — it never needed two implementations. Fast-vs-stable hashing/serialization
   collapses the same way (a parameter). Two orderings for a type is Rust's own established practice
   already avoiding this (`Reverse<T>` wrapper, `sort_by` comparator, never two `impl Ord for T`).

Once no case survived, adopted the full reversal rather than a narrower patch: coherence (borrowing
Rust's term directly) — at most one implementation of a concept per type, full stop, rejected
outright at declaration regardless of whether bindings would differ — and implementations became
unnamed, addressed by their (concept, type) pair alone, since naming only ever existed to
disambiguate multiples. `using` for evidence selection is gone with it. Rewrote the "Generics,
concepts, dispatch" section's core paragraphs in place rather than appending a correction on top:
the canonical/compact-JSON justification, the ambiguity-vs-declaration-site distinction, the
structural duplicate check, and every `using Name`/named-implementation reference throughout the
document (§1 inherent-implementation cross-reference, the `.` resolution-order passage, the CTFE
derive-coherence paragraph, the axiom examples, the dynamic-evidence-erasure passage, the
MissingProperty hook) — updated in place rather than left inconsistent with the new rule.

`feature_tour.md` updated to match: §15's `AccountEqualById`/`AccountEqualByAllFields` (the original
source of the canonical/compact-style justification, predating this session) rewritten to show the
rejection and its real resolution — a distinct wrapper type (`ById(Account)`), the Rust-idiomatic
answer — with the section's own former claim ("Finch deliberately doesn't do this... genuinely
different tradeoffs from Rust") corrected in place rather than left standing next to a contradicting
rule elsewhere in the same document. §19/§21/§22 updated mechanically (different concepts or
different generic instantiations for the same type were never affected by coherence, only stripped
of now-pointless names). §23 kept but reframed as design history — the structural-identity check it
described was a reasonable answer to the question actually asked at the time; a sharper, more basic
question, not a flaw in the check, is what superseded it.

Two new questions surfaced along the way, resolved without touching coherence: generic
implementations are exactly one declaration over the whole parametric family (not one per concrete
`T`), composing directly with the already-decided no-specialization rule rather than needing new
machinery — checked once against the family, immediately closing what would otherwise have reopened
D-style per-instantiation matching. And wanting per-type-shape optimization (a fast path
specifically for `u64`) without reopening specialization or coherence resolves through `match-type`
inside one implementation's body — already-established machinery, zero-cost once instantiated, since
static evidence already generates like a template and a `match-type` on a statically-known `T`
compiles to a dead-code-eliminated branch, not a runtime check; whether the remaining direct call
inlines away is ordinary backend inlining, no different from any other small direct call.

Two new gaps logged rather than guessed at: conditional/bounded generic implementations (Rust's
`impl<T: PartialEq> PartialEq for Vec<T>` shape — no established syntax), and coherence checking
across independently-compiled modules (whether Finch needs Rust's orphan-rule restriction on
implementing foreign concepts for foreign types, or defers the conflict to link time) — both added
to `feature_tour.md` §18.

**Continued 2026-09-18/19 — primitive numeric types, conversion, and overflow specified from
scratch, verified against a real D compiler and a real rejected D proposal rather than assumed.**
Shammah asked directly whether other holes remained, specifically naming primitives; checking turned
up a real, previously-unnoticed gap: the only integer/float types anywhere in the document were
`int`/`uint` (both fixed 64-bit, "initially") and `float` (fixed `binary64`, "initially") — no sized
family, and the semantic-profile section had explicitly flagged "arithmetic overflow" and "numeric
conversion" as things "the versioned specification must state" without ever actually stating them.
Also surfaced a real mistake of mine: `i64`/`u64`/`f64` had been used casually in several examples
this session as though already-established type names, when they weren't.

Design resolved through direct empirical and historical research rather than default assumption,
each piece checked before being written in:

- **Signed lengths/indices, not unsigned** — Shammah's own recollection of a blog-post argument
  (unsigned sizes/indices cause underflow-to-huge-value bugs) confirmed as a real, well-known
  position (Swift/C#/Java all chose signed deliberately; Rust/C++ kept unsigned and are the
  languages this exact critique targets), distinguished the two real arguments (underflow-to-huge
  vs. signed/unsigned mixing friction) from the weaker, more C/C++-specific "UB enables optimization"
  argument Finch's own no-UB ethos doesn't lean on.
- **D's value-range propagation, verified empirically against LDC, not assumed from memory** —
  confirmed real and genuinely better than C for width-narrowing (`byte b = 100;` compiles, `byte b
  = 300;` and non-constant narrowing don't), but confirmed to apply *no* check at all to same-width
  signed/unsigned conversion (`uint y = -1;` compiles silently) — a real gap in D's own safety story,
  not a hypothetical.
- **Walter Bright's actual position, read from the real rejected D proposal
  ("Deprecate implicit conversion between signed and unsigned integers")** rather than guessed: he
  declined the fix not because the bugs aren't real (he cites Stroustrup's 2018 "Subscripts and sizes
  should be signed" paper himself) but because "every proposal... cause[s] an equivalent number of
  new issues" specific to warnings breaking template-instantiation consistency across differently-
  configured modules, plus "D is a systems programming language, and trying to hide what the machine
  actually does usually results in awkwardness." His actual alternative is context-dependent
  (unsigned for never-negative values and pointer offsets specifically, signed otherwise), not
  blanket-signed — folded in as the deliberate exception for raw-pointer/FFI-boundary arithmetic
  rather than eliminating unsigned. Shammah's own framing: this is exactly why a from-scratch
  language can take the strict rule Bright rejected — no legacy corpus to break, and a hard compile
  error (not a warning) sidesteps the specific template-consistency problem he cited.
- **Explicit `cast`, never gated behind `unsafe`** — checked against the document's own already-
  established scope for `unsafe` (`unsafe.memory`, raw pointers, unverifiable FFI contracts,
  explicitly "unhosted native profile only") and confirmed numeric truncation belongs in neither
  category: fully deterministic and memory-safe, never able to corrupt memory or invoke undefined
  behavior, just potentially not the value intended — a real, different, and much weaker risk
  category than what `unsafe` actually gates elsewhere in the document. `as` was already taken for
  an unrelated pattern-binding form, so the new operator is `cast`, not `as`.
- **Overflow traps unconditionally, the same in every build** — deliberately not Rust's debug/release
  split, named directly as the same category of inconsistency this document already rejects elsewhere
  (a value's behavior depending on which path/configuration produced it, per the `core`/`core.thread`
  root-package bug and the `is()`/constraint-clause binding gotcha). Wrapping/saturating/checked
  variants remain available as named operations, never as an ambient build-mode toggle on `+` itself.

Added: `i8`/`i16`/`i32`/`i64`, `u8`/`u16`/`u32`/`u64`, `f32`/`f64` as the real sized-numeric family,
with `int`/`uint`/`float` as aliases for `i64`/`u64`/`f64` rather than the only names that exist —
retroactively validating rather than requiring a fix for this session's earlier `i64`/`u64`/`f64`
usage. Worked example added as `feature_tour.md` §24. The semantic-profile TODO bullet and the
"according to language policy" cross-reference in the work-packages section both updated to point at
the new content instead of a still-unwritten forward reference.

**Continued 2026-09-19 — FFI survey, logged as open questions rather than designed now.** Shammah
asked what other holes remain, specifically FFI into/out of Finch. Checked existing coverage before
concluding anything was missing: text/bytes crossing the boundary already has a real, coherent
architecture (views + owned-handle-with-release, native layouts never crossing the C/stable ABI),
and C-ABI scalars/variadic calling are already specified. Five real gaps surfaced and logged in
`feature_tour.md` §18 rather than designed in this pass, per Shammah's request to keep them easily
findable rather than resolve them immediately: panics/traps unwinding across an FFI boundary (more
urgent for Finch than for Rust specifically because overflow now traps unconditionally in every
build, unlike Rust's release-mode wrapping), creating an ordinary (non-variadic) callback from a
Finch closure, lifting C's sentinel-return-plus-`errno` convention into `result`/`throws`, reentrant
calls from a foreign thread the scheduler never spawned, and confirming `resource<K>` is the intended
mechanism for opaque foreign-handle wrapping rather than assuming it or inventing a second one later.
Build-time linking/library discovery noted as out of scope for this document (build tooling, not
language semantics), not logged as a gap.

**Continued 2026-09-19 — module/package/disk-layout system designed from scratch, D's real behavior
verified against LDC rather than trusted on reputation.** Shammah wanted D's package/module-to-disk
cleanliness specifically, and wanted CoLisp/Co-Forth fully interoperable rather than two separate
module systems. Checked existing coverage first: the document's own "modules and packages" section
turned out to be entirely about compiler-pipeline architecture (parse boundary, scheduler), and
"Initial module layout" turned out to be about the Rust *implementation's* own source tree, not a
Finch program's module system at all — genuinely nothing addressed file-to-module-path mapping
anywhere.

Verified D's actual behavior with real test files against LDC rather than assuming the reputation
was earned uniformly: `package.d` and `package`-level visibility are real and enforced (a nested
sub-package reading a parent's `package`-visible symbol compiled; a module outside the package tree
got a real "undefined identifier" error, not a warning) — genuinely worth adopting. But the "lays out
cleanly on disk" property turned out to be convention, not an enforced invariant, confirmed two
ways: normal `-I`-search compilation does resolve `import mypkg.foo` strictly by translating the
dotted path to `mypkg/foo.d`, but a file's own `module` declaration is a separate, hand-maintained
statement that can silently claim the wrong path (compiled with zero error when passed directly), and
*omitting* the declaration entirely falls back to the bare filename instead of the directory-derived
dotted path (confirmed: `import mypkg2.noheader` failed with "must be imported with 'import
noheader'"), quietly breaking the very property being relied on.

Design: module identity is derived entirely from file path relative to a source root — no separate,
restatable `module` declaration to write, omit, or let drift, closing the exact gap found in D rather
than copying it. `.colisp`/`.coforth` extensions established (neither existed before). A directory is
a package, importable via `package.colisp`/`package.coforth` (one per directory, either frontend).
`pkg` added as a third visibility tier between the existing module-private default and `pub`, checked
against directory-tree containment exactly as verified against LDC. Cross-frontend interoperability
falls directly out of already-established architecture rather than needing new machinery: since both
frontends already submit through one common elaborator into one typed IR ("One parse boundary,
modules, and packages"), a symbol's resolvability never depends on which frontend produced it — the
only genuinely new rule needed was that two files claiming the same module path in different syntaxes
(`mypkg/foo.colisp` and `mypkg/foo.coforth` both present) is a compile-time ambiguity error, the same
shape as coherence's one-implementation-per-(concept,type) rule. Worked example added as
`feature_tour.md` §25: a CoLisp module with a `pkg`-visible helper called directly from a Co-Forth
module in the same package, re-exported through `package.colisp`, with the `pkg` boundary enforced
against a caller outside the package.

**Continued 2026-09-19 — corrected the just-added module-identity design: source root ambiguity is
real, but the fix is the package-retrieval layer, not D's per-file declaration.** Shammah recalled
that D's redundant per-file `module` declaration exists for a reason — resolving which of several
`-I` search roots a file belongs to, including bare/direct file compilation with no search path at
all. That's a real, legitimate purpose, not one to dismiss the way the previous entry implicitly did
by proposing pure path-derivation with no declaration anywhere. But Finch already has a stronger
answer available than D's: the package-retrieval layer (lockfile/dependency declarations) already
gives an authoritative, non-guessed root-to-package mapping for every package in a build, including
the main project via its own manifest. Corrected the design to resolve a file's path relative to its
*owning package's* declared root (known from the dependency graph, resolved once per package) rather
than either D's per-file declaration or unresolved `-I`-order guessing.

**Continued 2026-09-19 — matrix/kernel-generation in CoLisp attempted as a real worked example,
stopped at the very first step by a genuine, previously-unnoticed foundational gap.** Shammah asked
about generating numeric kernels (matrix operations) easily in CoLisp. Rather than theorize, actually
tried writing a fixed-size `Matrix<T,R,C>` record using only already-established machinery and hit a
wall immediately: `array<T,N>` has always implied `N` is some kind of parameter, but no established
syntax anywhere declares an ordinary compile-time *integer* as a generic parameter — every generic
parameter shown anywhere in the document is a type. The one compile-time-value mechanism that exists
(`values xs : Ts...`) is a heterogeneous pack tied to a corresponding type pack, built for variadic
argument lists, not a single scalar dimension, and offers no way to compute `R * C` at the type
level. This blocks not just matrices but anything sized by a compile-time integer at all (fixed-
capacity buffers, small-vectors). Logged as feature_tour.md #26 rather than guessed past, alongside a
second, separate honest limitation: Cranelift (the document's own stated native backend) has weaker
auto-vectorization than LLVM, so whether Finch needs explicit SIMD lane types as a deliberate escape
hatch is a real, separate open question "generates like a template" doesn't already answer.

Separately, Shammah asked whether the module design mirrors Deno's or Go's system. Checked precisely
rather than agreeing loosely: it's closer to Go's actual mechanism (one manifest-declared abstract
module name, internal imports computed from it, fetching kept as a separate underlying concern) than
Deno's (where the import statement itself is the literal fetch URL, no separate abstract-name layer).
The "decentralized, hash-pinned, no mandatory registry" philosophy already in the document matches
both languages' shared spirit, but the specific mechanism is Go's, not Deno's — worth being precise
about since the two solve the same problem with a genuinely different abstraction boundary.

**Continued 2026-09-19 — relative imports and local dependency overrides added to the module system,
both real gaps in what was just designed.** Shammah asked about relative imports directly, and
separately raised local-override-for-a-remote-dependency without naming it — recognized as Go's
`replace` directive, a well-precedented feature (local development against an unpublished fork/fix).

Relative imports: `pkg` added as a relative anchor keyword (`pkg.foo` for a sibling module,
`pkg.super.foo` for the enclosing package), getting right what Rust's `crate::`/`super::` get right
and Go's actual modules do not — a Go internal import is always the module's full declared path, so
renaming a module means rewriting every internal absolute import. Deliberately did not adopt Go's
"same directory implicitly sees itself, no import needed" rule — an import is always required, kept
consistent with the explicit-over-implicit posture already established for concept implementations,
`cast`, and axioms. `pub`/`pkg` visibility apply identically whether a module is named absolutely or
relatively — `pkg.foo` is a different spelling of the same module, never a more permissive view of it.

Local override: the same manifest declaring the main project's own name/root may map a specific
dependency's locator to a local path instead of its normal source, matching Go's `replace`. The
scoping detail stated precisely since getting it wrong would leak local machine state into others'
builds: an override applies only when that manifest is the build's actual root — a dependency several
levels down whose own manifest records its own author's local override has that override ignored
entirely once consumed as a library, never inherited transitively.

`feature_tour.md` §25 updated in place: `ledger.coforth`'s import of its sibling `account.colisp`
changed from the absolute `accounts.account` to the relative `pkg.account`, demonstrating the new
form on the exact case it was designed for (a same-package, cross-frontend import).

**Continued 2026-09-19 — value-generic parameters designed and specified, unblocking the matrix-
kernel attempt from the previous entry.** `value N : int` added as a new generic-header form,
alongside plain type parameters (`<T, value N : int>`), deliberately unparenthesized unlike the
`(types Ts...)` pack form since a single value needs no grouping. Chose `int` for value-generic
sizes specifically for consistency with the already-established signed-length policy ("Numeric
types") rather than introducing a different-signedness exception for this one role.

Designed to need no new machinery beyond what already exists, checked against each established
mechanism in turn rather than assumed to compose: compile-time arithmetic on bound value parameters
(`(* R C)` for a matrix's flat storage size) reuses the same `! pure` CTFE-of-values evaluation
already established for everything else — deliberately not a separate type-level arithmetic
sublanguage, directly continuing the session's own thesis that ordinary syntax plus CTFE covers what
other languages need a template-metaprogramming layer for. Coherence and "generates like a template"
both apply to value-generic instantiations exactly as they already do to type-generic ones (each
concrete `(T,R,C)` is a distinct family member, monomorphized lazily, no new rule needed) — verified
against the same reasoning already used for `Codec<bytes,User>` vs. `Codec<User,User>` being distinct
instantiations rather than a coherence conflict. `infer` composes the same way too: a value parameter
derives from an argument's own already-bound value exactly as a type parameter derives from an
argument's own bound evidence. Added a `where` constraint form for value-generic parameters
(`value N : int where (> N 0)`), checked once at instantiation like any other declared bound, not a
second mechanism alongside concept bounds.

`feature_tour.md` §26 updated with the resolution: `Matrix<T, value R : int, value C : int>` now
typechecks, and a `matmul` function demonstrates `K` inferred from two independently-value-
parameterized arguments agreeing. Explicitly flagged what this does *not* resolve, rather than
overclaiming: the loop body is ordinary runtime iteration, not compile-time-unrolled, since
`ct-foreach` iterates a parameter pack, never a plain integer range — logged as a new, separate §18
gap (compile-time integer-range iteration) rather than conflated with value-generic parameters
themselves. Also flagged `for`/`range`/`+=`/`matrix-at`/`Matrix.zeroed` as unverified stdlib/syntax
per the document's own stated practice, joining the already-logged accumulating-stdlib-surface gap.
