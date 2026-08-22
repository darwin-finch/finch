# Typed Lisp, Co-Forth, Capabilities, and Error Pipeline Plan

## Status and relationship to existing plans

This document is the implementation plan for a single typed Finch language runtime with two
source syntaxes:

```text
typed Lisp source ────┐
                      ├──> Finch typed stack IR ──> verifier ──> interpreter
Co-Forth source text ─┘                                      └──> CLIF ──> native code (later)
```

It refines `SHARED_PROGRAM_RUNTIME_PLAN.md` and `VM_NATIVE_AGENT_RUNTIME_PLAN.md`. Where the
older shared-runtime plan says that Lisp must not be implemented by generating Forth text, this
plan makes the boundary precise: Lisp must not generate textual Forth and then be reparsed.
Both frontends compile directly to the same internal typed stack IR. Co-Forth remains a source
language with its own text syntax; it is not the IR. The shared vocabulary supplies the callable
semantics used by both frontends.

The current runtime is a migration starting point, not the target:

- the Co-Forth data stack is effectively `Vec<i64>`;
- stack effects count cells but do not validate their types;
- `ExecutionEffect` is a coarse ordered classification;
- the portable Lisp compiler emits Forth source text;
- unsupported Lisp falls back to the native Lisp evaluator;
- diagnostics are mostly strings;
- successful persistent execution is serialized by a per-session transaction gate.

The target removes the native Lisp fallback after semantic parity, replaces coarse effects with
resource-scoped capability sets, and gives interpreted and JIT execution the same verified IR,
transaction, and error behavior.

## Outcomes

1. Lisp and Co-Forth share values, vocabulary, functions, closures, capabilities, errors,
   scheduler primitives, and persistent VM state.
2. Incoming model programs are type-, stack-, effect-, authority-, and budget-checked before they
   can mutate state or request an external action.
3. The receiver independently verifies programs against its actual stack revision, vocabulary
   manifest, and grants; it never trusts a sender's claimed proof.
4. A human can approve exact or templated capabilities once, for a task, session, project, or
   global scope through precise dialogs backed by the same objects the runtime enforces.
5. Every error can be traced from a native instruction or IR operation through a Co-Forth word and
   originating Lisp form to the model submission and visible user turn.
6. Every provider receives a compact, versioned language definition plus on-demand vocabulary
   introspection, so it can write programs without relying on remembered words.
7. A later Cranelift backend lowers proven hot code through Cranelift IR (CLIF) without becoming a
   second semantic VM.
8. There is no process-wide GIL. Executions own their stacks and frames; shared state uses immutable
   versions, explicit transactions, concurrent handles, or narrowly scoped synchronization.

## Non-goals

- Do not implement the JIT before the typed IR and interpreter pass conformance tests.
- Do not promise that arbitrary reflective or self-modifying Forth is optimizable.
- Do not make all Lisp values boxed merely to simplify the frontend.
- Do not use source-text searches to infer effects or permissions.
- Do not use a shell as the implementation of filesystem, search, editing, automation, session,
  HTTP, or agent primitives.
- Do not persist authority inside arbitrary source strings, closures, prompts, or model output.
- Do not silently replay or compensate external effects when reverting VM state.

## Architectural decisions

### One semantic runtime, two source-language frontends

Finch typed stack IR is the common internal executable representation. It is a versioned
instruction stream with constants, typed stack operations, locals, lexical environments, calls,
branches, structured values, capability requests, suspension points, and returns. It is not a
third user-facing language and is not simply tokenized Forth text.

Co-Forth source compiles almost directly to the IR. Lisp is parsed, macro-expanded, type-checked,
closure-converted, and lowered by post-order traversal. Original source and spans remain attached
to the resulting instructions.

Users who explicitly enter Forth enter Co-Forth source text, for example `3 4 +` or a `:` word
definition. Users who explicitly enter Lisp enter Lisp source text. Ordinary conversational text
is not parsed as either language: Finch represents it as a typed string input to an agent turn
(conceptually `PUSH "..."`). Providers may return either supported source language through the
program-submission envelope. The relevant frontend parses and compiles that source before the
shared verifier sees it.

Traditional Forth implementations commonly compile text into threaded dictionary entries. Finch's
IR plays the same internal role while making types, control-flow blocks, source locations, effects,
and capability requests explicit enough to verify and later lower to native code.

The runtime retains canonical source in the authored language. Compiled IR, verifier summaries,
and native code are rebuildable caches keyed by source, compiler, vocabulary, dependency, target,
and policy hashes.

### Typed values use a hybrid representation

The language-level value model begins with:

```text
unit             no meaningful result
bool             true or false
int              signed 64-bit integer initially
uint             unsigned 64-bit integer initially
float            IEEE-754 binary64 initially
char             Unicode scalar value
string           immutable UTF-8 managed value
bytes            immutable or uniquely owned byte sequence
path<R>          normalized path proven relative to root R
list<T>          persistent or managed sequence
map<K,V>         managed mapping
option<T>        none or some(T)
result<T,E>      ok(T) or err(E)
record{...}      named product type
variant{...}     tagged sum type
word<S,E>        callable word with stack signature S and effects E
fn(A...)->R ! E  lexical closure
task<T>          scheduler-owned child/task handle
resource<K>      generation-bound runtime handle
capability<C>    unforgeable grant handle; never synthesized from text
dynamic          explicitly tagged escape hatch
```

Integers, booleans, floats, characters, and small handles should remain unboxed where the target
ABI permits it. Strings, collections, closure environments, and larger records use managed handles.
`dynamic` carries a tag and requires checked narrowing. Static code must not pay dynamic dispatch
cost merely because the Lisp frontend exists.

The serialized `ProgramValue` form is the wire/checkpoint representation, not necessarily the
in-memory stack layout.

### Typed stack signatures

Signatures use row polymorphism so a word states what it consumes while preserving unknown values
beneath it:

```text
dup          forall A S. (S A -- S A A) ! {}
drop         forall A S. (S A -- S) ! {}
+            forall S.   (S int int -- S int) ! {}
file.read    forall R S. (S path<R> -- S bytes) ! {fs.read(R)}
agent.await  forall T S. (S task<T> -- S result<T,agent-error>) ! {agent.await}
```

The signature includes:

- input and output stack rows;
- generic type parameters and constraints;
- control-flow behavior such as return, throw, or suspend;
- a capability/effect row;
- optional determinism, allocation, and numeric-overflow properties useful to optimization.

Definitions may declare signatures, but the compiler derives and validates them. Inferred public
signatures are stored in the vocabulary manifest. Unresolved calls, stack-dependent parsing, or
unsafe reflection prevent proof and require an explicit dynamic/unsafe boundary.

### Effects are capability requirements

A type describes values. An effect describes an observable action or authority requirement.
The target replaces a single ordered `ExecutionEffect` with a set of parameterized requirements:

```text
{}
{vm.read}
{vm.write(dictionary="session")}
{fs.read(root=workspace, path="src/**")}
{fs.write(root=workspace, path="generated/**")}
{network.connect(host="api.example.com", port=443)}
{automation.inspect(app="com.apple.Terminal")}
{automation.write(app="com.apple.Terminal")}
{agent.spawn(provider=["grok","claude"], max_depth=2, max_children=4)}
{process.run(executable="cargo")}
```

Effects are inferred from primitive calls and transitively composed through definitions. The core
authorization rule is:

```text
inferred requirements ⊆ submitted declaration ⊆ effective grants
```

The coarse effect classification remains temporarily as a UI risk summary derived from the set;
it is not the enforcement model.

### Resource selectors and templates

Capability selectors are parsed structured data, never interpolated commands. Each capability kind
owns a typed selector schema. Filesystem selectors support a deliberately small glob language:

```text
literal segment       src
single-segment glob   *.rs
recursive suffix      generated/**
root token            ${workspace}, ${project}, ${task.output}
```

`./**` is interpreted relative to an immutable capability root recorded in the execution context,
not the process's current working directory. Template variables are resolved by Finch when a grant
is created. Programs cannot redefine `${workspace}` or inject a new root.

Filesystem enforcement must:

1. parse and normalize the requested relative path;
2. reject parent traversal and invalid platform prefixes;
3. anchor resolution to an already-open or canonical capability root;
4. prevent symlink escape and check/use races with platform-appropriate relative-handle APIs;
5. match the normalized relative path against a compiled selector;
6. use the same resolved handle for the operation;
7. record the requested and resolved resource in the audit event.

Static paths can be proved during verification. Dynamic paths produce a runtime obligation. A
refinement such as `path<workspace:"generated/**">` discharges that obligation statically.

Function effects may contain a restricted selector expression over immutable typed arguments. The
allowed expression nodes are root, literal relative path, refined path argument, join, and narrow;
general string interpolation and user-defined evaluation are forbidden. Composition substitutes
the callee's argument expressions at each call site. Every expression also carries a conservative
selector upper bound. If substitution cannot prove a narrower selector, the caller inherits that
upper bound and the host operation checks the resolved resource at runtime. Rebinding a local does
not change an existing request: IR operands identify the specific immutable value version used by
the call.

Delegation computes intersections. A child can receive the same or a narrower selector and budget,
never a wider one. Begin with positive grants only; avoid allow/deny precedence until there is a
demonstrated need.

### Authority, availability, and effects remain distinct

- Effect requirement: what the program could request.
- Capability grant: what this execution is authorized to request.
- Availability: whether the host currently implements and can perform it.
- Approval policy: whether an otherwise valid request must suspend for user consent.

A word may type-check and be authorized while still returning `CapabilityUnavailable` because OS
Accessibility permission was revoked or a provider disappeared. Availability changes increment the
environment generation.

## Typed Co-Forth language definition

The exact surface grammar will be frozen through an RFC, but the language contract must include the
following constructs.

### Definitions and signatures

Illustrative syntax:

```forth
: square ( S int -- S int ! {} )
  dup *
;

: save-report
  ( S path<workspace:"generated/**"> string -- S unit
    ! {fs.write(workspace:"generated/**")} )
  file.write
;
```

Signatures are compiler-readable, not comments. A compatibility reader may initially accept classic
`( ... )` comments, but verified definitions store a parsed `Signature` object.

### Locals, quotations, and closures

Provide explicit locals for generated code and readable handwritten definitions:

```forth
: hypotenuse { x:int y:int -- float }
  x x * y y * + int>float sqrt
;
```

Quotations are typed callable values:

```forth
[ int -- int ! {} | 1 + ]
```

An escaping quotation is closure-converted into an immutable code reference plus a managed captured
environment. Calls use `call`/`tail-call`; they do not create an untyped anonymous stack.

### Control flow

`if/else/then`, loops, pattern matching, early returns, and exception/result operations must have
explicit IR blocks. Every merge point requires compatible typed stacks. Loops require a stable
stack invariant. Arbitrary jumps are not part of verified source.

### Dynamic and unsafe boundaries

Reflection and legacy words may be retained behind explicit boundaries:

```text
dynamic.call       requires runtime signature check
unsafe.memory      requires an unsafe-memory capability and cannot be remotely granted by default
legacy.eval        unclassified; interpreted only; explicit approval
```

Unsafe/dynamic words cannot be silently inlined into a verified pure definition.

## Typed Lisp language definition

### Semantic profile

Define a small Finch Lisp rather than claiming complete Common Lisp or Scheme compatibility. The
versioned specification must state:

- eager left-to-right argument evaluation;
- lexical scope;
- proper tail calls where marked by the IR;
- exact behavior of truth, `nil`, equality, arithmetic overflow, and numeric conversion;
- immutable-by-default collections;
- mutation only through typed references with explicit `vm.write` effects;
- exceptions versus `result<T,E>` behavior;
- supported macro phase and hygiene rules;
- absence or presence of continuations, dynamic scope, multiple values, and reader extensions.

Initially exclude general continuations and unrestricted reader/runtime `eval`. Add them only with a
clear typed/effect model.

### Functions and annotations

Illustrative syntax:

```lisp
(define (square (x : int)) : int
  (* x x))

(define (save-report
          (path : (path workspace "generated/**"))
          (contents : string))
  : unit
  ! (effects (fs/write workspace "generated/**"))
  (file/write path contents))
```

Local inference should make annotations optional inside functions. Public definitions require an
inferred or declared stable signature before publication.

### Lowering

The frontend performs:

1. parse with exact source spans;
2. hygienic macro expansion in a restricted compile-time environment;
3. name resolution and lexical binding;
4. Hindley-Milner-style local inference plus explicit effect rows and practical subtyping/refinement
   checks where needed;
5. desugaring of `let`, `begin`, `if`, pattern matching, and named functions;
6. closure conversion and capture analysis;
7. post-order lowering into typed stack blocks;
8. tail-call marking;
9. common verifier invocation.

For example:

```lisp
(+ 3 (* 4 2))
```

lowers directly to IR equivalent to:

```text
const.int 3
const.int 4
const.int 2
call core.mul
call core.add
return
```

It does not construct `"3 4 2 * +"` and re-enter the Forth text parser.

### Macros

Macros run before runtime capability checking and receive syntax objects, not ambient host access.
Macro expansion must have its own fuel, recursion, and allocation limits. Expansion provenance maps
generated forms back to both macro invocation and macro definition. A macro cannot hide effects:
the expanded IR is what the verifier analyzes.

## Common typed IR

Create a stable internal model resembling:

```text
Module
  version
  constants
  type table
  capability requirements
  imports by immutable ProgramRef
  functions
    signature
    locals/captures
    basic blocks
    instructions with SourceOrigin

Instruction examples
  Const, Dup, Drop, Pick
  LocalGet, LocalSet, CaptureGet
  RecordNew, FieldGet, VariantNew
  Call, CallClosure, TailCall, Return
  Branch, CondBranch, Match
  CheckedAdd, CheckedDiv, Convert
  HeapAllocate
  CapabilityRequest
  SpawnTask, AwaitTask, CancelTask
  Suspend, Resume
  Trap
```

Every instruction declares a typed stack transformation and effect contribution. Program imports
resolve to immutable IDs and versions before verification. The IR serializer is versioned and
rejects unknown mandatory instructions.

The verifier proves:

- instruction and call operand types;
- compatible stack rows at control-flow merges;
- loop invariants;
- initialized locals and valid captures;
- signature agreement on every return;
- transitive effects and capability selector containment;
- valid immutable dependency versions;
- bounded static limits where available;
- absence of forged handles or capabilities;
- well-formed suspension and resumption types.

Verifier output is a reusable certificate summary keyed to the exact module hash. Remote peers may
send summaries for caching, but each receiver verifies the module independently.

## Runtime, memory, and concurrency

Each execution owns:

- a data stack;
- call frames and return continuations;
- typed locals and temporary roots;
- a cancellation token and budget counters;
- an effect journal and pending transaction;
- its capability-grant reference;
- task/agent ancestry and source context.

Code modules, type descriptions, source maps, and published vocabulary versions are immutable and
shareable. Session dictionary updates use versioned transactions. Concurrent executions begin at a
declared VM revision and explicitly commit compatible deltas; they do not mutate one global stack.

Managed values initially use Rust-owned reference-counted immutable objects and uniquely owned
builders for efficient construction. Cyclic mutable structures should either be excluded initially
or placed in a per-runtime tracing heap with explicit safepoints. Do not add a process-wide collector
lock. If tracing GC is introduced, prefer per-runtime/per-arena collection plus immutable cross-arena
handles, and document which operations are safepoints.

Builders are first-class implementation APIs for strings, bytes, lists, diagnostics, and patches.
Compilation and rendering must not repeatedly replace or concatenate whole strings for incremental
construction.

## Capability broker and approval pipeline

### Request lifecycle

```text
verified instruction
  → instantiate typed capability request
  → compare with execution grants
  → check host availability
  → consult approval policy
  → execute, suspend for approval, or reject
  → record structured outcome
  → resume with typed result
```

The same `CapabilityRequest` value supplies enforcement, dialog rendering, persistence, audit, and
child delegation. Human-facing prose is presentation only.

### Approval choices

Dialogs should offer only scopes valid for the request:

```text
deny
allow once
allow for this task
allow for this session
allow for this exact resource in this project
allow for an editable suggested pattern in this project
allow globally, only when policy permits
```

The dialog displays agent ancestry, program/source hash, normalized resource, requested operation,
reason, current matches where safe, and the difference between exact and wider suggested grants.
Broad selectors such as workspace `**`, wildcard network hosts, desktop mutation, credentials,
process execution, and recursive agent spawning receive prominent warnings.

Persisted decisions store a typed selector, root identity, operation, scope, source/policy binding,
creator, timestamp, and revocation state. They never store only the display string. The UI provides
searchable history and immediate revocation.

### Transaction rule

A persistent VM execution builds a delta. It commits stack/dictionary/heap changes only when:

- verification succeeded;
- all required synchronous capabilities completed successfully;
- no uncaught error or cancellation occurred;
- its expected VM revision still matches or its delta merges without conflict.

External effects are journaled execute-once facts and are never claimed to roll back with VM state.
Suspension preserves a typed continuation and transaction; resumption rechecks environment,
manifest, grant, program, and resource generations before continuing.

## Structured error pipeline

### Error phases

Use one diagnostic envelope across:

```text
reader
macro expansion
name resolution
type inference
stack/effect verification
linking/manifest validation
authorization
availability/approval
interpretation
native/JIT execution
transaction commit
child-agent execution
cancellation/resource limits
```

### Diagnostic model

Replace `Vec<String>` with structured diagnostics while retaining a formatted compatibility view:

```text
Diagnostic
  stable code                 e.g. E-TYPE-002, E-CAP-004
  severity                    note | warning | error
  phase
  concise message
  primary SourceOrigin
  related origins
  expected and found values/types/stacks/effects
  capability request and effective grant summary
  VM, manifest, dependency, and environment revisions
  word/function and inlining trace
  agent/task ancestry
  safe typed stack snapshot
  remediation hints
  nested cause
```

`SourceOrigin` can identify Lisp spans, Forth spans, macro expansions, generated IR operations,
stored program versions, model message/tool calls, and native instruction ranges. Sensitive values
are redacted according to type and policy; secrets are never copied into diagnostics by default.

### Error propagation

- Expected operational failure is a typed `result<T,E>` when callers commonly recover.
- `throw`/trap is for exceptional failure and unwinds typed frames to the nearest compatible handler.
- Uncaught errors abort the VM transaction and become a failed `ExecutionOutcome`.
- Child failures remain structured inside `agent-result`; the master may inspect, retry, summarize,
  or propagate them without scraping text.
- Cancellation and fuel/time/memory exhaustion are distinct stable error kinds.
- Approval denial is not reported as a compiler error; it is an authorization outcome.

Interpreted frames record word IDs and IR offsets. Optimized code uses explicit side exits and
metadata maps. Do not dedicate a permanent machine register to a global error flag, and do not rely
on the Forth return stack to reconstruct optimized/inlined calls.

## Provider-facing language definitions

### Canonical artifacts

Add versioned, generated-and-checked documentation artifacts:

```text
vocabulary/language/FINCH_VM.md       shared values, effects, errors, execution contract
vocabulary/language/FINCH_FORTH.md    Forth syntax and examples
vocabulary/language/FINCH_LISP.md     Lisp syntax and examples
vocabulary/language/schema.json       machine-readable type/capability/diagnostic schemas
vocabulary/language/conformance/      small executable examples and expected outcomes
```

`vocabulary/BOOT.md` becomes a compact capsule generated from the normative definitions. It must
state protocol/version hashes, the action envelope, how to introspect, and the safety rules. It must
not attempt to list the full vocabulary.

### Runtime manifest

Every fresh model, provider switch, child agent, and context compaction receives:

```text
language and IR versions
BOOT capsule and normative spec hashes
current VM/manifest/environment revisions
typed top-of-stack summary with stable positions
available capability kinds and current grants/availability
relevant word names, typed signatures, effects, and one-line documentation
limits and child identity
introspection tool schemas
```

Full word documentation, source, examples, and tests are fetched on demand through vocabulary
inspection. Prompt construction selects relevant entries rather than dumping a growing dictionary.

### LLM-oriented requirements

The language definitions must be concise, literal, and executable:

- one canonical syntax per construct;
- no examples using unavailable or invented words;
- explicit stack direction and top-of-stack notation;
- exact string/path escaping rules;
- examples that begin with pre-existing stack values;
- examples for `PUSH <natural-language text>` and returning a program;
- capability declaration and approval examples;
- child spawn/await/cancel examples;
- common diagnostics with corrected programs;
- a rule to inspect vocabulary rather than guess;
- manifest/revision requirements on every submission.

Generate provider prompt fragments and schemas from the canonical vocabulary registry so prose,
tool schemas, verifier signatures, and runtime words cannot silently drift.

## Cranelift JIT plan (deliberately later)

### IR layering

Cranelift IR is conventionally called CLIF. It is a distinct, lower representation from Finch's
typed stack IR:

```text
Finch typed stack IR
  semantic types, stack effects, capability requirements, suspension, source origins
        ↓ verified lowering
CLIF
  SSA values, blocks, calls, guards, loads/stores, target-independent machine operations
        ↓ Cranelift code generation
native code
```

Finch IR is the durable semantic and verification boundary. CLIF is target/backend-oriented and
normally a rebuildable compilation artifact. Do not serialize CLIF as the program-exchange ABI or
ask models to generate it. Capability authority is already validated before lowering, but every
runtime shim call remains capability-bound so malformed or stale native artifacts cannot bypass the
broker.

Lowering emits a side metadata table that CLIF alone cannot represent completely. It maps CLIF
blocks/instructions and resulting native ranges to Finch IR offsets, Lisp/Forth source origins,
inline frames, trap kinds, safepoints, transaction state, and capability request sites.

### Prerequisites

Do not begin native code generation until:

1. typed IR format and interpreter semantics are stable and versioned;
2. the verifier rejects malformed stack/control/effect programs;
3. closures, managed handles, traps, cancellation, and capability calls have stable runtime ABIs;
4. source maps and structured errors work in the interpreter;
5. differential and transaction tests are established;
6. word/dependency versioning supports reliable invalidation.

### Tiering

Use three tiers:

```text
tier 0: verified IR interpreter
tier 1: cached baseline Cranelift compilation for hot functions
tier 2: optional optimized recompilation using profiles and proven specialization
```

Collect per-word call counts, loop back-edge counts, type specialization observations only at
`dynamic` boundaries, execution time, and deoptimization/trap counts. Compilation happens off the
execution fast path when practical. Cold, reflective, unsupported, or rapidly changing code stays
interpreted.

### Native ABI and lowering

- Lower verified Finch IR blocks into CLIF blocks and map virtual stack slots to CLIF SSA values.
- Spill only across calls, control-flow merges, suspension points, and register pressure.
- Eliminate `dup`, `swap`, `over`, and local stack shuffles in SSA when possible.
- Lower checked arithmetic with explicit overflow/division side exits according to language policy.
- Call stable Rust runtime shims for allocation, capability requests, task operations, and complex
  managed-value operations.
- Use safepoints/stack maps if a tracing heap is introduced.
- Preserve cancellation/fuel polling at verified loop and call boundaries.
- Follow the platform ABI; do not permanently reserve a global error register.

### Errors and deoptimization

Every native code range maps to module/function/IR offset, Forth origin, Lisp origin, and inline
frames. Guards branch to shared typed trap stubs. If speculative specialization is later added,
failed guards reconstruct an interpreter frame at a declared deoptimization point. Native and
interpreted execution must produce equivalent diagnostics and transaction outcomes.

### Cache and invalidation

Native artifacts are keyed by:

```text
IR hash
compiler and Cranelift versions
target triple and CPU feature set
runtime ABI version
dependency ProgramRefs
type/effect certificate hash
relevant policy mode
```

Dictionary redefinition creates a new immutable word version. It never patches old callers to new
semantics accidentally. Direct calls to immutable dependencies stay valid; alias-based dynamic
lookups remain interpreted or use guarded indirection.

Use platform W^X memory handling and never leave pages simultaneously writable and executable.
Remote compiled code is never accepted as trusted; peers exchange source/IR and the receiver
verifies and compiles it locally.

### JIT acceptance gates

- differential interpreter/JIT results across generated typed programs;
- identical error codes, origins, and rollback behavior;
- no capability bypass through native shims;
- cancellation and budget compliance for native loops;
- sanitizer/fuzz coverage for ABI and trap boundaries;
- measurable improvement on representative hot vocabulary, not microbenchmarks alone;
- automatic fallback to interpretation after compilation failure.

## Implementation work packages

### Phase 0: Freeze contracts and fixtures

- Write RFCs for value representation, typed signatures, capability selectors, errors, Lisp
  semantics, and Co-Forth syntax.
- Add canonical language artifact directories and version fields.
- Capture existing useful Forth/Lisp programs as migration and conformance fixtures.
- Inventory every builtin and assign its current cell effect, intended typed signature, effects,
  suspension behavior, and migration status.

Exit: reviewers can answer what any core word consumes, produces, and may do.

### Phase 1: Typed core model

- Introduce `Type`, `TypeVar`, `StackRow`, `Signature`, `EffectSet`, `CapabilityRequirement`,
  `ResourceSelector`, `TypedValue`, and stable IDs.
- Keep `ProgramValue` as serialization and add checked conversions.
- Implement selector parsing, canonical rendering, containment, and intersection.
- Derive the coarse risk classification from effect sets for compatibility.

Exit: unit/property tests cover type substitution and selector algebra.

### Phase 2: Typed IR and verifier

- Define versioned modules, functions, blocks, instructions, source origins, and imports.
- Implement virtual typed-stack verification and control-flow merging.
- Infer transitive effect rows and verify declarations/grants separately.
- Produce structured verifier diagnostics and certificate summaries.
- Add parser/IR/verifier fuzz targets and malformed-module tests.

Exit: unverified IR cannot enter either execution backend.

### Phase 3: Co-Forth frontend and interpreter

- Compile core Co-Forth syntax to typed IR.
- Add real parsed signatures, locals, quotations, call frames, and typed stack values.
- Bind builtins through a generated typed registry rather than a hand-maintained name/effect split.
- Execute verified IR while preserving current vocabulary behind migration adapters.
- Add transactional stack/dictionary state and expected-revision commits.

Exit: migrated core words pass interpreter conformance tests without `Vec<i64>` assumptions at the
language boundary.

### Phase 4: Capability broker and dialogs

- Replace string capabilities and source-text effect inference with typed registry metadata.
- Implement filesystem selector hardening and runtime obligations first.
- Add grant lifetimes, persistence, revocation, attenuation, and audit storage.
- Suspend/resume execution around approval dialogs using typed continuations.
- Route native files/search/edit/network/automation/agent/process operations through the broker.

Exit: enforcement, dialog display, persisted grant, audit event, and delegation use the same
serialized capability object.

### Phase 5: Structured error and transaction pipeline

- Introduce diagnostic codes, phases, origins, traces, redaction, and nested causes.
- Change `ExecutionOutcome` and agent results to structured diagnostics.
- Guarantee rollback of VM-local changes on uncaught error/cancellation/conflict.
- Journal external effects separately and expose partial-effect failures honestly.
- Render concise user errors with expandable technical details in the shadow-buffer UI.

Exit: every failure phase has golden user rendering and machine-readable assertions.

### Phase 6: Typed Lisp frontend

- Specify the supported Finch Lisp semantic profile.
- Implement macro expansion, lexical resolution, inference, desugaring, closure conversion, and
  direct typed-IR lowering.
- Add managed closure environments and tail calls.
- Bind Lisp names to the same immutable vocabulary entries and capability primitives as Co-Forth.
- Differentially test portable old-evaluator programs during migration.

Exit: supported Lisp never emits Forth text, and closures/locals/capabilities run in the common VM.

### Phase 7: Provider language package

- Write and validate `FINCH_VM.md`, `FINCH_FORTH.md`, `FINCH_LISP.md`, schemas, and examples.
- Generate `BOOT.md`, prompt fragments, vocabulary summaries, and tool schemas from canonical data.
- Add handshake refresh on provider change, compaction, environment change, and stale submission.
- Test multiple providers on a fixed suite of stack-aware programming tasks.

Exit: a provider with no Finch-specific training can inspect the VM and produce valid programs at a
measured target rate without full vocabulary injection.

### Phase 8: Remove compatibility paths

- Reject or explicitly sandbox untyped definitions that cannot be migrated.
- Remove source-spelling effect inference.
- Remove native Lisp fallback after semantic and persistence parity.
- Remove legacy direct model tools after VM-native equivalents meet compatibility gates.
- Keep explicit versioned import/conversion tools for old stored programs.

Exit: production Lisp and Co-Forth share one verified execution engine and capability broker.

### Phase 9: Concurrency hardening

- Replace broad persistent-session execution serialization with revisioned VM transactions and
  conflict-aware commits.
- Keep execution-local stacks/frames lock-free from unrelated executions.
- Stress task/agent fork, await, cancellation, capability attenuation, and concurrent vocabulary
  publication.
- Establish heap ownership and collection behavior without a process-wide GIL.

Exit: independent tasks scale across worker threads and state conflicts are explicit outcomes.

### Phase 10: JIT instrumentation and Cranelift prototype

- Add stable runtime shim ABI, hotness counters, native cache keys, and source-map storage.
- Implement typed-stack-IR-to-CLIF lowering for a pure arithmetic/control-flow subset.
- Validate emitted CLIF with Cranelift's verifier before native code generation.
- Differentially test against the interpreter and measure real workloads.
- Expand to managed values and runtime calls only after trap/safepoint correctness.

Exit: the JIT is optional, capability-safe, observably faster on selected hot paths, and removable
without changing language behavior.

## Testing strategy

Every phase adds tests at the layer where its invariant is enforced:

- parser and source-span golden tests for both syntaxes;
- type inference, unification, stack-row, branch-merge, and loop-invariant tests;
- effect derivation, selector normalization, containment, intersection, and adversarial path tests;
- compile-fail fixtures with stable diagnostic codes and spans;
- transaction rollback, stale revision, suspension/resumption, and external-effect journal tests;
- child authority attenuation and cross-branch authorization tests;
- serialization compatibility and corrupted IR/manifest rejection tests;
- property tests generating well-typed and deliberately ill-typed IR;
- fuzzing for readers, IR decoder, verifier, selectors, and capability request decoding;
- provider conformance tasks using only the supplied language package;
- interpreter/Lisp-lowering differential tests during migration;
- interpreter/JIT differential tests when the JIT exists;
- UI snapshots for approval, denial, compile error, runtime trap, child failure, and revocation;
- platform security tests for symlinks, races, Unicode paths, case sensitivity, and root changes.

CI must test the typed runtime with automation unavailable, enabled-but-ungranted, granted, and
revoked. JIT-enabled and interpreter-only configurations run the same conformance corpus.

## Migration and compatibility policy

1. Assign every existing builtin a generated typed registry entry before changing execution.
2. Treat unknown legacy stack signatures/effects as dynamic and unclassified, never pure.
3. Compile existing vocabulary in report-only mode and publish incompatibility diagnostics.
4. Add adapters for legacy integer string/resource indexes while moving callers to managed handles.
5. Version persisted definitions and retain their original runtime requirement for replay.
6. Never silently reinterpret an old program under new word definitions or language semantics.
7. Provide automated rewrites only when source and effect behavior are provably preserved.
8. Remove the native Lisp fallback only after conformance, closure, macro, capability, persistence,
   and diagnostic parity gates pass.

## Initial module layout

The exact names may change, but ownership should remain clear:

```text
src/vm/types.rs                 type/value model
src/vm/signature.rs             typed stack rows and inference primitives
src/vm/effects.rs               effect sets and capability requirements
src/vm/selectors.rs             resource selector parsing and algebra
src/vm/ir.rs                    versioned typed IR
src/vm/verifier.rs              stack/type/effect verifier
src/vm/interpreter.rs           verified IR interpreter
src/vm/heap.rs                  managed values and roots
src/vm/transaction.rs           VM deltas, revisions, commit/rollback
src/vm/diagnostic.rs            structured errors and source origins
src/vm/capability_broker.rs     authorization, suspension, invocation, audit
src/coforth/frontend/           typed Co-Forth parser and lowering
src/lisp/frontend/              expansion, inference, closure conversion, lowering
src/jit/clif_lowering.rs       later Finch-IR-to-CLIF lowering
src/jit/                       later Cranelift ABI, native cache, traps, source maps
vocabulary/language/            canonical provider-facing definitions
```

Keep `src/runtime` as orchestration around the VM: submissions, manifests, execution contexts,
scheduler, provider resolution, and projection into session/UI events.

## Definition of done

The project reaches the intended architecture when:

- one typed IR and verifier define runtime semantics;
- both Lisp and Co-Forth compile directly to it;
- the native Lisp fallback is gone;
- public words expose checked typed stack/effect signatures;
- capability selectors are structured, scoped, attenuable, persistable, revocable, and audited;
- approval dialogs enforce exactly the grant they display;
- failures are structured and traceable to original source across both languages and native code;
- model language packages are generated, versioned, discoverable, and pass provider conformance tests;
- independent executions and agents do not require a process-wide GIL;
- the interpreter remains the reference implementation;
- the optional Cranelift tier passes differential, security, cancellation, transaction, and
  performance gates without changing observable language behavior.
