# Finch language implementation plan

## Outcome

Implement [`SPECIFICATION.md`](SPECIFICATION.md) as one language with two source syntaxes, one
semantic-construction boundary, one verified IR, and one reference execution semantics. The
exploratory [`FINCH_LANGUAGE_DESIGN.md`](FINCH_LANGUAGE_DESIGN.md) supplies rationale; it is not an
alternative compiler contract.

This plan is dependency ordered. Milestones are semantic gates, not release dates. A milestone is
complete only when its normative rules have executable conformance fixtures and every predecessor
path named by its tickets has been deleted.

## Starting point

The shared compiler boundary and span-preserving frontend work already exist. The repository has:

- `finch-language` as the compilation facade;
- private phase-state types through `ModuleVerified`;
- CoLisp and Co-Forth frontend crates isolated from execution;
- a typed IR, verifier, and reference interpreter; and
- a paired core conformance corpus.

The remaining work is not a rewrite from zero. It replaces duplicated frontend semantics and fills
the missing normative contracts in vertical slices while keeping the current core suite green.

## Non-negotiable implementation rules

1. A shared semantic node and checker rule land before convenience syntax in either frontend.
2. Every shared feature has paired CoLisp/Co-Forth fixtures and one principal negative fixture.
3. Frontends submit typed construction events; they never mint HIR, evidence, effects, or IR
   certificates.
4. The verifier independently derives security- and ownership-relevant facts. It does not trust
   frontend claims.
5. Only `ModuleVerified` executes. Native code remains a rebuildable cache of verified semantics.
6. Every temporary path names its owner, successor, deletion trigger, and deletion proof.
7. Serialized source, syntax, interfaces, IR, checkpoints, and native caches carry independent
   versions.
8. No milestone closes with two semantic paths for the same feature.
9. Performance gates accompany semantic assertions; they never replace them.
10. Implementation issues quote the specification rule/fixture they implement, not a rationale-only
    passage from the design history.

## Required artifacts before feature implementation

### S0 — specification harness

Goal: turn the specification into executable review boundaries before broad M1 work.

Formal-artifact status:

- [x] assign stable rule families to `SPECIFICATION.md` sections and deterministic IDs to grammar
  productions in `spec-rules.json` and `grammar/*.json`;
- [ ] expand the core conformance corpus with the now-closed scalar, resumable-exception,
  namespace/path, and ABI decisions; the formalization artifacts required by findings 24, 32, and
  35 are checked in, but broad positive/negative/hostile fixtures still gate implementation readiness;
- [x] check in complete machine-readable lexical, type, pattern, expression, and core-form grammars for
  both syntaxes rather than relying on declaration-schema prose;
- [x] replace the bootstrap [`spec-prelude.json`](spec-prelude.json) with a generated, signature-AST-bearing
  registry checked into the repository;
- [ ] execute the canonical prelude AST through an independent consumer oracle; callable and concept
  parameters, typed generic bounds, evidence, ownership, projections, loan origins, contract axes,
  effect rows, selectors, and named trusted-law expressions are structural and range-checked;
- [x] freeze the type relation, bottom/join rules, exception-pattern subtraction, callable-predicate
  truth table, closure capture, assignment order, scalar semantics, and bounded-resource outcome;
- [x] define canonical semantic-construction event JSON and semantic digest formats;
- [x] make every transition rule executable from paired parsed sources through normalized events,
  including its cleanup and protected paths: a reference machine interprets the rule programs
  directly, 100 execution vectors assert complete trace/terminal/state, and
  `transition-coverage.json` requires every instruction and declared branch of all 36 rules to run;
- [x] execute both normative grammars against an accept/reject corpus that exercises every
  production and pins each rejection's reader code and byte offset;
- [x] specify IR version 6 as the implemented version 5 plus exception/cleanup regions, explicit
  drops, slot references, and tail calls (`semantics/ir.json`), with a reference lowering,
  structural verifier, and executor that reproduce the rule machine on every vector;
- [ ] extend `finch-vm-core` and `finch-vm` from IR version 5 to version 6 against those vectors;
- [ ] give the static semantics (types, ownership, effect rows, exception sets, evidence) an
  executable checker with accept/reject vectors; today only the 27 static rejections a dynamic
  machine can observe are covered, and the differential fixture IR certifies control flow only;
- [ ] split and complete machine-readable native-call, serialized-message, target, replay, and callback ABI schemas;
- [ ] extend `vocabulary/language/conformance/core.json` with `spec_rule`, `profile`, `stage`, expected
  diagnostic, and optional semantic/IR digest fields;
- [ ] add a corpus validator that rejects unknown rules, missing paired sources, duplicate case IDs,
  malformed expected outcomes, and an active profile with zero cases;
- [ ] add reader-only golden suites for both syntaxes, including explicit JSON literals and rejection of
  parse-success fallback;
- [ ] record the exact language, syntax, interface, and IR versions in every compiler result.

`scripts/language/check_language_spec.py` checks the completed artifact rows. The unchecked rows are
implementation/corpus integration work; they are not unresolved language decisions.

Exit gate: one deliberately malformed fixture fails at corpus validation, one syntax error fails at
the reader boundary with the expected code, and one paired valid program reaches identical semantic
digests and observable results.

This gate should become its own issue before language feature work is dispatched. It is documentation
and test infrastructure, not permission to implement all later semantics in one ticket.

## Dependency graph

```text
S0 specification harness
 ├─> M1A core kinds/types/diagnostics
 │    ├─> M1B modules + semantic scheduler + inference
 │    ├─> M1C places + ownership + drop + layout
 │    └─> M1D effects + exceptions + local-state masking
 │          └──────────────┐
 ├─────────────────────────┼─> M2A CTFE + syntax/hygiene
 M1B ──────────────────────┤
 M1C ──────────────────────┤
                           ├─> M2B generics + concepts + evidence
                           └─> M2C derives + tests + required prelude
 M1C + M1D + M2B ───────────> M3 resumable execution + ranges + concurrency
 M1–M3 verified semantics ───> M4 portable ABI + AOT + self-hosting
 M4 + benchmarks ────────────> M5 native optimization
 M2–M5 evidence ─────────────> M6 accelerator extension (separate profile)
```

File overlap imposes an additional mutex. Work touching the same construction/checker/IR node or
artifact version is serialized even when dependency edges would otherwise permit parallel work.

## M1 — semantic kernel

### M1A — core semantic model and diagnostics

Implement kinds, type identities, callable contracts, nominal records/variants, tuples, primitive
numeric rules, source origins, and the structured diagnostic envelope in the syntax-neutral core.

Required proof:

- type/kind well-formedness and normalization tests;
- forward-only inference blame-locality tests;
- numeric conversion/overflow fixtures;
- record/variant layout and pattern-exhaustiveness fixtures; and
- decoded IR cannot forge phase or verification state.

Relevant backlog:

- [#68 — bounded directional inference and interface publication](https://github.com/darwin-finch/finch/issues/68)
- [#675 — coherent text and sequence values without representation coercion](https://github.com/darwin-finch/finch/issues/675), limited initially to the value/type foundations it requires.

### M1B — modules and dependency-driven semantic jobs

Implement package-root-derived module identity, lexical imports, immutable interfaces, interned jobs,
phase promises, deterministic cycles, cancellation, cache keys, and interface publication.

Required proof:

- repeated/concurrent imports share one semantic job;
- local import visibility begins at the declaration and cannot re-export;
- private/package/public visibility works across CoLisp and Co-Forth;
- scheduling order produces byte-identical interfaces and diagnostics; and
- dependency replacement invalidates exactly the affected identities.

Relevant backlog:

- [#66 — typed modules, immutable interfaces, and decentralized imports](https://github.com/darwin-finch/finch/issues/66)
- [#67 — dependency-driven semantic compiler scheduler](https://github.com/darwin-finch/finch/issues/67)
- [#68 — bounded directional inference and interface publication](https://github.com/darwin-finch/finch/issues/68)

### M1C — places, ownership, lifecycle, and layout

Implement place paths, initialization, loans/reborrows, control-flow joins, whole-value moves and
consuming destructures, copy, exclusive mutation, deterministic cleanup, owner concepts, weak
upgrade, cross-task transfer evidence, and `repr(native|C|stable N)`.

Deliver the source spellings exactly as specified: CoLisp default borrow, `borrow-mut`, and `steal`;
Co-Forth source modes plus lowering-only `consume-value`; mutable locals and `record-set!`.

Required proof:

- nested projection and reborrow cases;
- mutation during shared borrow and overlapping dynamic-index rejection;
- branch-join initialization, rejected field moves, and consuming-destructure cleanup behavior;
- closure escape and suspension rejection;
- exact cleanup order on success, throw, trap, and cancellation;
- `Shared`/`Weak` hostile concurrency and upgrade/drop races; and
- safe code cannot transfer or share a type without the corresponding evidence.

Relevant backlog:

- [#674 — ownership, placement, deterministic drop, and safety profiles](https://github.com/darwin-finch/finch/issues/674)

This issue should be split into checker/lifecycle slices under one integration owner; it is too broad
for one unreviewed patch.

### M1D — effects, exceptions, suspension contracts, and capabilities

Implement canonical effect rows, row variables/union/containment, fresh local-state regions and
masking, typed exception summaries and handler subtraction, suspension summaries, callable
predicates, selector AST/algebra, and verifier-derived contracts.

Required proof:

- open-row generic propagation and duplicate-label canonicalization;
- local unique mutation can remain pure while escaped/shared mutation cannot;
- partial catch subtracts only handled exception types;
- a loan cannot cross a possible suspension;
- `join`/`narrow` never widen a selector;
- inferred request ≤ declaration ≤ live grant ≤ policy; and
- capability effects cannot be masked or forged through evidence/imports.

Relevant backlog:

- [#83 — one extensible effect row for capabilities and control metadata](https://github.com/darwin-finch/finch/issues/83), whose body must be reconciled with the specification's separate exception/suspension summaries and local-state masking before implementation.
- [#84 — typed exceptions and scope guards](https://github.com/darwin-finch/finch/issues/84)

### M1E — sequences, text, and test declaration reserve

Complete arrays/slices/vectors/lists/bytes/string contracts over M1C ownership and M1D effects.
Reserve syntax-neutral test/suite nodes and discovery identities without yet implementing concept-
driven matchers or mocks.

Required proof:

- no implicit allocation/encoding/representation conversion;
- escaped views fail;
- text equality/hash are segmentation-independent;
- byte/scalar/grapheme units are not interchangeable;
- test discovery executes no module code; and
- test declarations are absent from production artifacts.

Relevant backlog:

- [#675 — coherent text and sequence values without representation coercion](https://github.com/darwin-finch/finch/issues/675)
- [#677 — named tests, typed matchers, fixtures, and mocks](https://github.com/darwin-finch/finch/issues/677), declaration/discovery slice only.

M1 exit: representative multi-module programs in both syntaxes produce identical semantic digests,
verified IR, ownership/effect diagnostics, and sealed interfaces.

## M2 — staging, generics, concepts, and language-level testing

### M2A — bounded CTFE and hygienic syntax

Implement deterministic CTFE, scope-set syntax objects, typed capture call nodes, `mixin`, reader
decorators, compile-time hooks, `type->syntax`, `function-spec-of`, `fresh-name`, `ct-range`, and
compile-time iteration. Remove `macro:` and the restricted template macro path after migrations pass.

Required proof:

- no textual reparse or host capability during CTFE;
- scope introduction/use-site resolution and ambiguity fixtures;
- dynamically selected syntax-capturing callable rejection;
- nested CoLisp/Co-Forth transformations produce equal expansion trees;
- cache keys include every immutable semantic input; and
- fuel, recursion, allocation, and expansion failures are deterministic.

Relevant backlog:

- [#78 — pure bounded CTFE and reflection kernel](https://github.com/darwin-finch/finch/issues/78)
- [#79 — hygienic CTFE syntax transformations replacing template macros](https://github.com/darwin-finch/finch/issues/79)

### M2B — parametric HIR, generics, concepts, and evidence

Implement type/value/pack parameters, forward deduction, checked-once bodies, lazy concrete
generation, associated projection/normalization, coherent evidence, the orphan rule,
`stable-evidence`, axioms, and static/dynamic dispatch.

Required proof:

- unconstrained generic bodies fail before instantiation;
- value parameters participate in identity and cache keys;
- independently compiled foreign/foreign implementations fail at declaration;
- overlapping bounded implementations remain forbidden;
- associated cycles terminate with an actionable diagnostic;
- stable evidence keys cannot be reused across versions; and
- interpreter behavior is identical for static and explicitly erased paths where both apply.

Relevant backlog:

- [#80 — source-defined parametric generics over typed HIR](https://github.com/darwin-finch/finch/issues/80)
- [#81 — coherent concept evidence and parameter packs](https://github.com/darwin-finch/finch/issues/81), whose older structural-matching wording is superseded by explicit implementations.

### M2C — derivation, attributes, records, and full test language

Implement structured reflection/derivation, namespaced attributes, generated declaration/evidence
checks, and the remainder of named tests: typed matchers, fixture factories, static/dynamic doubles,
host-effect fakes, isolation, property tests, and snapshots.

Required proof:

- generated code has definition/use origins and no private ambient reach absent invocation-site
  consent;
- derivations cannot replace published declarations or evade coherence;
- matchers evaluate a subject once and retain source spans;
- fixture cleanup is exact under failure/cancellation/timeout; and
- parallel tests have fresh state/capability/task trees unless a stable shared key is declared.

Relevant backlog:

- [#82 — typed record rows, schemas, and CTFE derivation](https://github.com/darwin-finch/finch/issues/82)
- [#85 — uniform namespaced attributes](https://github.com/darwin-finch/finch/issues/85)
- [#677 — named tests, typed matchers, fixtures, and mocks](https://github.com/darwin-finch/finch/issues/677), remaining slices.

Run [#86 — scripting ergonomics and model-repair metrics](https://github.com/darwin-finch/finch/issues/86)
before freezing version 0.1 surface syntax. Metrics may motivate reader sugar but cannot create a
second semantic path.

M2 exit: a user record can derive code/evidence, satisfy a coherent concept, instantiate generic
code, run bounded transforms, and test the result identically from both syntaxes.

## M3 — resumable execution, ranges, and concurrency

Implement the private resumable state machine, policy wrappers for fibers/tasks/streams, bounded
buffering/backpressure, cancellation/replay, required task ownership, range concepts/adapters, and
safe synchronization with explicit happens-before contracts.

Finding 30's policy is closed: `task<T,X>`, `stream<T,X>`, and `fiber<Y,Resume,R,X>` carry the
producer's canonical `ExceptionSet` in `X`, while protected terminal outcomes remain outside it.
The transition rules are executed by the reference machine, and the fiber, task, effect, and
cancellation vectors in `fixtures/execution-vectors.json` are the conformance oracle this stage must
reproduce. Not yet vectored: cancelling a child parked mid-run, grant revocation between a child's
dispatches, and the `join-all`, `race-and-reap`, and `select-complete` combinators.

Required proof:

- yielded and terminal states cannot be confused;
- every handle has exactly one terminal transition;
- cancel/join/race-and-reap/select preserve remainder ownership;
- drop/reaper behavior is exact under timeout, restart, and replacement;
- buffered producers enforce item and byte bounds;
- task joins hide internal parks while generators expose semantic yields;
- range adapters do not receive a privileged built-in path; and
- safe programs cannot contain a data race.

Relevant backlog:

- [#93 — private typed resumable execution substrate](https://github.com/darwin-finch/finch/issues/93)
- [#94 — scheduler policies and typed combinators](https://github.com/darwin-finch/finch/issues/94)
- [#97 — bounded streaming and range vocabulary](https://github.com/darwin-finch/finch/issues/97)

M3 exit: generators, tasks, effectful sources, and ranges use one verified substrate without
`async`/`await` source coloring or conflated policies.

## M4 — portable ABI, AOT oracle, and self-hosting

Freeze the portable ABI only after M1–M3 contracts stabilize. Implement opaque handles,
pointer-length views, owned results, diagnostics, effect/resume envelopes, trap lockdown,
`ForeignCallback`, foreign-thread attachment, foreign resources, explicit sentinel/`errno` wrappers,
and safe/unhosted profile separation.

Finding 32's path canonicalization, target identity, hosted/unhosted policy, and registry tuple are
closed. Findings 52–53 require split native/serialized ABI schemas, replay/callback state machines,
structured interface signatures, and exact identity binding before C and Go host fixtures can freeze
the ABI.

Build AOT from verified semantics, then stage-zero/bootstrap reproducibility and the self-hosted
compiler service.

Required proof:

- no unwind crosses the ABI;
- callback revocation/lifetime/thread policy is enforced;
- foreign reentry enqueues a root invocation and never enters an existing frame;
- C and Go hosts compile/execute the same fixture and own/release every buffer correctly;
- interpreter and AOT results, traps, drops, and effects agree;
- stage 1 and normalized stage 2 agree from the audited seed path; and
- failed stage publication leaves the prior generation active.

Relevant backlog:

- [#90 — portable runtime/application ABI](https://github.com/darwin-finch/finch/issues/90)
- [#99 — `finchc` AOT on verified compiler/runtime contracts](https://github.com/darwin-finch/finch/issues/99)
- [#101 — qualified native performance and expressiveness benchmarks](https://github.com/darwin-finch/finch/issues/101)
- [#102 — reproducible self-hosted frontend and semantic scheduler](https://github.com/darwin-finch/finch/issues/102)

M4 exit: hosts consume one versioned compiler/runtime image, and self-hosting changes implementation
language without changing the contract.

## M5 — optimizing native backend

Add selective specialization, inlining, representation optimization, and the compact self-hosted
native tier only behind interpreter differential gates. Algebraic rewrites require certified laws
and preserve strict numeric/effect behavior. Publish qualified measurements; do not infer
performance from architecture.

M5 exit: backend selection is unobservable except for resource use and declared implementation
limits.

### Performance risk register and gates

Performance is a cross-milestone acceptance condition, not an M5 cleanup task:

- S0 measures parser/event/digest throughput and proves the normal compiler path need not serialize
  JSON or allocate one object per semantic transition;
- M1 measures cold and one-line incremental builds, peak memory, evidence candidates visited, row and
  exception normalization work, generic instantiations, and emitted-code growth;
- M2 measures CTFE and expansion work, cache reuse, and invalidation fan-out;
- M3 measures task/fiber state size, suspension/resume latency, cancellation cleanup, `Unique` versus
  `Shared` operations, and the cost of enabled journaling/checkpointing;
- M4 measures portable-ABI encoding, calls, callbacks, handles, and release traffic separately from
  internal calls; and
- M5 measures interpreter and native throughput, allocations, code size, branch/check elimination,
  and compile-time cost of each optimization.

Every benchmark reports semantic configuration, input scale, warm/cold state, target, toolchain,
median and tail latency, peak memory, and dated baseline. A regression gate must pair performance
with result/IR/effect/drop equivalence. Features with unavoidable costs—ordered drop, observable
explicit retain/release traffic, suspension, grapheme traversal, portable encoding, and external-effect
journaling—must have explicit pay-for-use comparisons; they are not averaged into unrelated code.

## M6 — accelerator extension

Treat tensor/kernel/SIMD semantics as a separately versioned profile. Define typed shapes/layouts,
address spaces, aliasing, transfers, barriers, atomics, divergence, target limits, and source maps
before direct device emission. Start with an established backend and CPU differential oracle.

Relevant backlog:

- [#676 — verified tensor and accelerator kernel substrate](https://github.com/darwin-finch/finch/issues/676)

M6 exit: representative kernels match the CPU oracle under their numeric policy and reject
bounds/race/synchronization violations before execution.

## Verification ladder for every ticket

Run the smallest relevant gate first and stop on the first failure:

1. fixture/schema validation;
2. reader or semantic-builder unit tests;
3. checker/verifier unit tests, including the principal invalid case;
4. paired frontend semantic/IR conformance;
5. interpreter production-boundary regression;
6. serialization/restart/hostile timing tests when durable or concurrent state changes;
7. reverse-consumer suites selected from actual imports;
8. formatting, linting, interface generation, and repository hygiene; and
9. benchmark/budget guard only when the ticket owns one.

All Brain, daemon, server, TUI, and live-process tests run through `scripts/test_brains.sh` or an
approved supervised launcher. Test daemons bind loopback port zero and remain in the supervisor's
process group.

## Portable worker packet

Each implementation ticket must state:

- user-visible outcome and specification rule IDs;
- exact base revision and integration branch;
- writable/prohibited files and semantic ownership;
- predecessor path and deletion trigger;
- paired positive/negative fixtures;
- verifier and execution proof;
- artifact-version impact;
- risk tier and review requirements; and
- handoff containing change, verification, remaining risk, owner, and next dependency.

The default language scope is `docs/language/**`, `vocabulary/language/**`, the language/compiler
crates, and focused language integration tests. Provider, TUI, Brain, persistence, and host-policy
implementation are outside scope unless a ticket names the integration explicitly.

## Backlog reconciliation required before dispatch

Several open issue bodies predate the accepted design. Before claiming them, update their solution
contracts to reference this specification. In particular:

- coherent concepts are explicit implementations, not structural method discovery;
- exceptions and suspension share one source `!` contract presentation but retain distinct typed
  summaries; they are not forgeable capability labels;
- local uniquely owned mutation is maskable state, not a host capability and not automatically
  observable impurity;
- CoLisp has no source `consume-value` parameter; and
- `macro:` and per-implementation `dynamic-evidence-version` are retired.

Backlog tooling named by the triage workflow is not present in this worktree as of 2026-10-01, so
dependency ordering above was reconciled manually from live issue bodies and the specification.
Restore or replace that tooling before automated worker dispatch; do not pretend the mechanical
readiness/conflict audit ran when it did not.
