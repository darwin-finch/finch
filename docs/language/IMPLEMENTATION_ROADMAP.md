# Finch language implementation roadmap

## Outcome

Make Finch language work a bounded program that an agent can execute without loading provider,
Brain, TUI, persistence, or application-runtime implementations. Implement the language
incrementally against [`FINCH_LANGUAGE_DESIGN.md`](FINCH_LANGUAGE_DESIGN.md), while establishing
semantic seams early enough that ownership, concepts, macros, effects, resumability, native code,
and accelerator work do not require replacing both frontends again.

This is an implementation sequence, not a second language specification. When it conflicts with the
language design, resolve the design explicitly before writing compatibility machinery. Finch is
pre-stable: migrate checked-in programs and delete superseded compiler paths rather than preserving
a known wart solely because it was implemented first.

## Measured starting point

Measured on `main` at `ae251bc4` (2026-09-13):

| Crate/file | Lines | Current language responsibility |
|---|---:|---|
| `finch-vm-core` | 7,242 | types, signatures, IR, verifier, diagnostics, vocabulary, effects, and host authorization |
| `finch-colisp/src/frontend.rs` | 4,678 | CoLisp semantic analysis and direct IR lowering in one file |
| `finch-coforth/src/compiler.rs` | 3,812 | Co-Forth parsing, semantic analysis, and direct IR lowering in one file |
| `finch-vm/src/interpreter.rs` | 2,891 | reference execution plus frontend-dependent tests |
| `finch-vm/src/runtime.rs` | 3,949 | compilation selection, checkpoints, effects, and runtime orchestration |

The existing dependency direction is useful: both frontends depend only on `finch-vm-core`, and
`finch-vm` composes them. The remaining problem is semantic, not the number of crates: both
frontends independently perform work that should become one syntax-neutral compiler pipeline, and
`finch-vm-core` contains host authorization state that is not part of language compilation.

## Target ownership and dependency graph

Use five coarse responsibilities. Names describe the target; rename or move crates only after the
dependency boundary exists in code.

```text
                     finch-colisp ──┐
finch-language-core ←───────────────┼─→ finch-language
                     finch-coforth ─┘       compiler facade/orchestrator
          ↑
          └──────────────────────────── finch-vm
                                           verified execution only

root Finch application ──→ finch-language + finch-vm + host runtime/policy
```

- **`finch-language-core`** (currently `finch-vm-core`) owns source identity/spans, semantic types,
  ownership modes, effect rows, concepts/evidence, syntax-neutral semantic construction, parametric
  HIR where necessary, typed IR, module phase types, diagnostics, and independent verification. It
  owns capability *requirements* because they are typed effects, but not grants, approval policy,
  authorization ledgers, or UI choices.
- **`finch-colisp`** owns only the CoLisp reader, syntax objects, surface grammar, and translation to
  the semantic-construction protocol. It does not infer types/effects, resolve concepts, or build
  final IR privately.
- **`finch-coforth`** owns only the Co-Forth reader, stack-oriented syntax objects, surface grammar,
  and translation to the same semantic-construction protocol. It has no separate semantic subset.
- **`finch-language`** becomes the compilation facade and dependency-driven orchestrator. It chooses
  a frontend, registers declarations/modules, schedules elaboration and CTFE jobs, seals interfaces,
  and returns a verified module. It is allowed to be small initially because it owns a durable
  integration boundary that grows with the compiler rather than a one-function extraction.
- **`finch-vm`** consumes `ModuleVerified` values and owns the reference interpreter, resumable
  execution machinery, and execution ABI. Source compilation is injected by the application; the
  VM does not select or call a frontend. Host policy/checkpoint orchestration may remain temporarily
  behind its compatibility facade, with a named owner and deletion trigger, until application
  runtime extraction is ready.

After those imports prove the graph, group the four compiler crates beneath one
`crates/language/` capsule if the repository ownership checker and measured agent context show a
benefit. Do not perform a path-only move first: directory shape should record a real boundary, not
promise one.

## Rules that prevent the expensive second rewrite

1. A feature gets one syntax-neutral semantic node/contract before either frontend implements its
   convenience syntax.
2. Every common feature lands with paired CoLisp and Co-Forth fixtures, or is explicitly marked as
   a frontend-only reader feature that lowers to an existing common node.
3. Frontends call typed semantic builders. They never manufacture final verified types, evidence,
   effects, module states, or IR certificates.
4. `Parsed`, `Elaborated`, `FunctionCertified`, `ModuleSealed`, and `ModuleVerified` are distinct
   Rust types with private transitions. Only `ModuleVerified` reaches execution.
5. The common model reserves explicit homes for ownership/placement, concepts/evidence, effect rows,
   exception edges, suspension signatures, patterns, layouts/ABI, parameter packs, and source
   provenance before implementing their surface sugar. Do not encode any of these as strings,
   frontend-interpreted decorators, or ad hoc builtin names.
6. The retained AST and small parametric HIR are extensible structured data, not repeatedly rewritten
   source. Lower to concrete typed stack IR once the information required by verification is known.
7. The interpreter exhaustively consumes verified instructions. A new instruction or contract
   changes the verifier and reference execution in the same integration slice.
8. Serialized source, interfaces, IR, checkpoints, and caches carry explicit versions. Before stable
   release, reject or migrate old artifacts; never silently reinterpret them.
9. A temporary predecessor path names its owner, immediate successor, removal trigger, and deletion
   proof in the implementing issue. A stage is not complete while both semantic paths remain.
10. Optimize only after equivalence is established. Cranelift, a self-hosted backend, and GPU
    kernels consume certified semantics; none may become a second type/effect system.

## Staged implementation

### M0 — establish the compiler boundary

Goal: one place to add language semantics before adding more surface area.

- Execute [#673 (shared compiler boundary)](https://github.com/darwin-finch/finch/issues/673) as
  the integration tracker for this milestone.
- Finish #65 (single authoritative parse and retained span-preserving AST).
- Define the versioned semantic-construction protocol and opaque phase-state types.
- Introduce the `finch-language` compilation facade and move frontend selection out of
  `TypedRuntime`; submit `ModuleVerified` to execution.
- Separate host authorization/grant policy from language-level capability requirements.
- Move cross-frontend conformance tests to the compilation boundary and keep execution equivalence
  in `finch-vm` integration tests.
- Split the two large compiler files only along stable jobs—reader/syntax, construction,
  elaboration, lowering, and tests—not into single-function files.

Exit: adding a shared semantic node does not require private implementations in both frontends, and
`finch-vm` can execute a verified module without depending on either reader.

### M1 — semantic kernel and published modules

Goal: freeze the semantic shapes on which later features depend.

- Implement #66 (typed modules and immutable interfaces), #68 (bounded directional inference), and
  #67 (dependency-driven semantic jobs) on the shared pipeline.
- Make imports ordinary lexical declarations with whole-module, namespace-alias, selective, and
  renamed forms; local visibility begins at the declaration and never creates runtime loading or
  ambient authority.
- Intern immutable module identities and deduplicate parse/semantic jobs and verified artifacts by
  versioned content/dependency keys; repeated scoped imports create binding views, not recompilation.
- Keep modules immutable and make runtime state an explicit owned instance; provide at-most-once
  initialization through a library `Once<T>`/service policy with explicit lifetime, failure,
  cancellation, effect, and suspension semantics.
- Establish nominal records, variants, tuples, callable contracts, layouts, sequence/text values,
  patterns, structured diagnostics, and module compatibility hashes.
- Implement [#674 (ownership, placement, deterministic drop, and safety profiles)](https://github.com/darwin-finch/finch/issues/674)
  before closures, suspension, or FFI make their representation costly to alter.
- Implement [#675 (coherent text and sequence values)](https://github.com/darwin-finch/finch/issues/675)
  on those ownership/layout foundations without representation coercion.
- Run #86 (language ergonomics and model-repair metrics) before freezing surface syntax.

Exit: representative multi-module programs type-check through both syntaxes with identical semantic
artifacts, ownership diagnostics, and sealed interfaces.

### M2 — concepts, effects, and metaprogramming

Goal: make advanced facilities ordinary compositions over one compiler kernel.

- Implement #78 (bounded CTFE), #79 (hygienic syntax transformations), #80 (parametric HIR and
  generics), #81 (concept evidence and parameter packs), and #82 (records/schema derivation).
- Implement operator evidence and certified algebra laws through concepts; do not add member-only or
  builtin-only dispatch paths.
- Implement #83 (one extensible effect row), #84 (typed exceptions and scope guards), and #85
  (uniform namespaced attributes).
- Delete the restricted template macro path after migration fixtures pass.

Exit: a user-defined record can derive an implementation, satisfy a concept through explicit
evidence, instantiate generic code, and produce identical verified IR from CoLisp and Co-Forth.

### M3 — ranges and resumable execution

Goal: prove that collections, generators, fibers, green tasks, I/O waits, and compiler jobs share
coherent mechanisms without sharing inappropriate policies.

- Implement #93 (private `ResumableExecution<Y,Resume,R>`) and #94 (scheduler policies and
  combinators).
- Implement #97 (bounded ranges and streaming vocabulary) over concept evidence.
- Preserve explicit terminal versus yielded states, bounded buffering/backpressure, ownership of
  unfinished handles, deterministic cleanup, and checkpoint contracts.
- Keep compiler `Needs` jobs structurally related but semantically distinct from runtime fibers.

Exit: generator, task, and range examples require no `async`/`await` coloring, have paired syntax,
and pass deterministic cancellation/replay/cleanup tests.

### M4 — stable compiler service and self-hosting

Goal: make the Rust compiler replaceable without replacing its contracts.

- Freeze #90 (portable runtime/application ABI).
- Build #99 (verified AOT compiler) and #101 (qualified native benchmark suite).
- Implement #102 (stage-zero bootstrap and reproducible self-hosted semantic scheduler).
- Preserve one stable C ABI so Rust, Go, and other embedders can load the same verified compiler
  image and receive structured diagnostics.

Exit: stage 1 and stage 2 produce normalized equivalent compiler artifacts, and hosts can compile
and execute through the versioned ABI without embedding Rust compiler internals.

### M5 — optimizing native and accelerator backends

Goal: exploit certified semantics without changing them.

- Add profile-guided selective specialization and the compact self-hosted native tier.
- Implement [#676 (verified tensor and accelerator kernel substrate)](https://github.com/darwin-finch/finch/issues/676):
  typed tensor/kernel HIR, accelerator verification, scheduling policies, bounded autotuning, and
  an established GPU backend before considering direct device-code emission.
- Gate every algebraic rewrite on certified evidence and strict numeric/effect equivalence.
- Compare against mature CPU and tiled-GPU systems on compile latency, throughput, diagnostics,
  portability, and source size before making replacement claims.

Exit: backend selection is an optimization choice with interpreter-equivalent semantics and safe
fallback, not a language fork.

## Scoped language-agent packet

A coordinator can delegate one ready language issue with only:

- this roadmap and the relevant section of `FINCH_LANGUAGE_DESIGN.md`;
- the `AGENTS.md` and `INTERFACE.md` for the affected language crates;
- the issue contract, base revision, precise files, predecessor/removal trigger, and focused gates;
- paired source fixtures and the smallest affected boundary tests.

Default writable scope is `docs/language/**`, `crates/finch-vm-core/**`,
`crates/finch-colisp/**`, `crates/finch-coforth/**`, and language integration tests under
`crates/finch-vm/tests/**`. Changes to `finch-vm` execution internals, `vocabulary/language/**`, the
root application, wire/checkpoint formats, or provider prompts are explicit integration scopes and
select their reverse consumers. Provider, TUI, Brain, persistence, and host-policy implementations
are otherwise out of scope.

The worker returns the semantic change, paired syntax status, verification, predecessor deletion,
remaining risk, and next dependency—not an exploration transcript.

## GitHub organization

Every language issue receives the umbrella `area/language` label plus the narrowest applicable
component labels: `language/frontend`, `language/semantics`, `language/ir-verifier`,
`language/runtime`, or `language/toolchain`. Use `language/colisp` or `language/coforth` only for a
genuinely syntax-specific issue; shared features receive neither.

Milestones reflect dependency gates rather than release promises: **Language M0 — compiler
boundary**, **Language M1 — semantic kernel**, **Language M2 — concepts and metaprogramming**,
**Language M3 — resumable execution**, **Language M4 — self-hosting**, and **Language M5 — native
and accelerators**. Existing value/certainty/unblocking/cost scoring and readiness rules still choose
work within the earliest ready milestone. A milestone or label never makes an unready issue ready.
