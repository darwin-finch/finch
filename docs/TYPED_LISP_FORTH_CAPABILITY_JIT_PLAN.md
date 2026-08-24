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

### Progressive output templates

`say` is an effect-producing word whose events can be consumed by a session event loop. Literal
and computed fragments can already be emitted as separate typed chunks. A future Co-Forth
quasiquote/template syntax may package those chunks together, but each embedded expression must
lower to ordinary typed IR before execution; textual interpolation and implicit evaluation are
not permitted.

`say` appends to the response port bound to its ProgramRun. Rich presentation is intentionally
separate: `output-open` receives a host-issued opaque output handle and `output-append`,
`output-replace`, `output-status`, `output-progress`, `output-complete`, and `output-fail` emit
ordered portable `Ui` effects targeting that handle. The VM never selects a global "active work
unit". A shadow-buffer terminal, IDE, web client, or accessibility host maps the same handle events
to its own rendering model and validates handle ownership/generation before projection.

### Wire syntax and self-contained scripts

The provider wire protocol has a deliberately cheap dispatch rule: a response whose first
non-whitespace byte is `(` is Finch Lisp; every other response is Co-Forth. The dispatcher does
not guess from prose and the provider prompt says that user-visible prose must be emitted by a VM
operation such as `"hello" say`, never written outside the program. Empty responses and Markdown
fences are explicit malformed-wire diagnostics with corrective guidance, not accidental Co-Forth
words. An explicit `language` field in a stored script/tool submission overrides this compact
streaming discriminator. Co-Forth is therefore the natural streaming form: the receiver can parse
and render complete tokens while waiting for later tokens, whereas Lisp remains preferable when
nested structure makes the leading `(` worth it.

Bare `"text"` is the preferred short escaped-string literal in Co-Forth. `s"text"` remains a
Forth-compatible equivalent and has no implicit leading space: both `s"text"` and conventional
`s" text"` produce `text` because the one delimiter whitespace is consumed. Spacing belongs in
the literal or is composed explicitly (for example `space`, `str-cat`, or separate `say` events).
`"""..."""` is the preferred raw multiline/prose literal; compatible `s"""..."""` also works.
It preserves its contents verbatim until the next triple quote, avoiding fragile quote escaping in
user-visible text. Co-Forth uses `\\` line comments. Parenthesized Co-Forth comments are allowed only after a
Co-Forth token: at the start of a wire response, `(` selects Lisp. The normative language definition
must give exact escaping and raw-delimiter examples.

Finch scripts are portable, self-contained source artifacts rather than shell wrappers. A script
may begin with a normal Finch shebang such as `#!/usr/bin/env finch --exec`; `finch --exec` selects
the strict typed frontend and must never silently fall back to the legacy Forth or Lisp
interpreters. Scripts may choose Lisp or Co-Forth using the same first-token rule. Imports and
namespaces are a later package feature: model-emitted one-off scripts should normally be complete
and auditable in one file. Bash, Python, and other external scripts remain valid *proposal
artifacts* when they are the appropriate user-editable delivery format; Finch scripts do not
remove that capability.

### Single-pass parsing, modules, and packages

Single-pass parsing is a hard language constraint. Each Lisp or Co-Forth module's source byte
stream is lexed/read exactly once into span-carrying syntax or direct lowering events. Subsequent
macro expansion, name resolution, type inference, optimization, linking, and independent IR
verification operate on retained structured data; none may rescan the source or serialize code and
reparse it. The independent verifier remains mandatory because it proves the produced IR rather
than interpreting source a second time.

Source order is independent from this parsing constraint. During the one parse, the frontend
registers every top-level declaration skeleton before semantic jobs require its body, so later
definitions are valid forward references. The dependency scheduler resolves them on demand.
Explicit signatures are required for exported module interfaces, genuinely ambiguous inference,
or cycles that cannot otherwise reach `SignatureReady`—not merely because a callee appears later
in the file. Macros are bounded structured transformations, not context-sensitive token
reinterpretation. A parser never guesses and revisits an earlier token after discovering a later
declaration; the retained AST and symbol registry carry that information into semantic analysis.

Single-pass parsing does not mean emitting final IR directly from lexer tokens. Each frontend must
produce an explicit, span-preserving syntax tree in that one source pass. Lisp already has the
beginnings of this boundary in `Val` and `SpannedVal`. Co-Forth now performs one tokenization into a
span-preserving module tree whose definition and top-level bodies retain an ordered node sequence
and lower against the original source; it no longer copies/masks and re-tokenizes those bodies.
Anonymous quotations are recursive parser-owned body nodes containing their signature and body,
rather than delimiter ranges in a byte-offset side table rediscovered or skipped during IR
emission. Integers, booleans, symbols, strings, and pasted JSON are classified as typed literal
nodes in the same source pass, so IR emission no longer discovers literals by reinterpreting word
text. Every other body element is retained as an explicit unresolved-word node rather than a
generic atom. Elaboration must turn those words into structured control nodes and resolved
local/call references before this gate closes. Syntactic sugar, macros, and other rewrites
operate on those nodes, and one post-order semantic lowering emits the common typed stack IR.
Generated syntax retains both its call-site and definition origin and is never converted to text
and reparsed.

Keep that pipeline deliberately short:

```text
source bytes -- one reader/parser pass --> frontend AST
frontend AST --> declarations + typed module interfaces
frontend AST -- elaboration/expansion --> parametric HIR (only where required)
elaborated AST/HIR -- instantiation + post-order lowering --> typed stack IR
typed stack IR -- independent security verification --> executable module
```

Shared lowering helpers enforce Lisp/Co-Forth parity without requiring an intermediate tree for its
own sake. Source-defined generics and compile-time templates are the concrete feature that can
justify one small shared parametric HIR: a concrete typed runtime instruction stream is too late to
retain generic parameters, constraints, an unresolved reusable body, module-interface references,
and expansion provenance. The HIR may instead be an explicitly elaborated AST if no separate node
family is useful. It must not become an excuse for a succession of mandatory compiler passes.
Optimization may traverse retained IR and never changes the one-pass source contract.

Modules are compilation units, never textual includes. A module has an immutable identity, typed
imports and exports, a namespace, a compiled interface, IR, source map, and content hash. Importing
a module links its declared interface/IR; it does not paste source, execute ambient initialization,
or confer capabilities. Self-contained model-authored scripts remain the default when an import
would make an artifact harder to audit.

Semantic analysis should be dependency-driven rather than implemented as repeated whole-module
passes. After the one parser pass registers declaration skeletons, each symbol and generic
instantiation owns a bounded semantic job with monotonic readiness phases:

```text
Declared -> SignatureReady -> BodyTyped -> Lowered
```

A job that requires another symbol at a particular phase yields an explicit compiler continuation
such as `Needs(symbol_id, SignatureReady)`. The scheduler advances the dependency and resumes the
requester. Generic instantiation creates or reuses a synthetic job keyed by immutable module and
definition identity plus its type/value arguments. Phase-aware dependency traces distinguish legal
mutual recursion, whose declared signatures break the cycle, from impossible compile-time value or
layout cycles and report the complete chain with source origins.

This is the useful architectural lesson from [SDC's semantic
scheduler](https://github.com/snazzy-d/sdc/blob/master/src/d/semantic/scheduler.d): its source uses
stackful fibers to make `require(symbol, phase)` read synchronously while dependent symbols advance
on demand. Finch should initially implement the same dependency semantics with explicit resumable
compiler jobs rather than native fiber stacks. That preserves deterministic scheduling, cycle
diagnostics, fuel limits, and straightforward tests/serialization. Compiler continuations are an
internal frontend mechanism and are not language-level `fiber<Y,R>` values.

Package retrieval is a separate later layer over modules. Dependency declarations identify a
source locator and exact version or immutable content hash, and a checked-in lockfile fixes the
complete transitive graph. Resolvers must support local paths and decentralized Git, HTTPS, and
content-addressed sources; a future Finch or third-party registry may be a discovery index, mirror,
or cache but must not be required infrastructure or the authority for package identity. Resolution
verifies hashes and, when available, signatures/provenance before compilation, prevents dependency
confusion, and never runs ambient install scripts or grants runtime authority.

The repository now contains the first verified typed path: both frontends lower directly to typed
IR, the typed runtime owns a `Vec<TypedValue>` stack, effects are resource-scoped capability
requirements, diagnostics carry stable codes, and host execution is transactional. Ordinary
`ProgramRuntime`, provider, scheduler, and script submission are typed-only. The native Lisp
evaluator and its effectful standard library have been removed; the retained Lisp reader lowers
only into shared typed IR. Public `: ... ;` and `/forth` source execution also enter only the typed
runtime. The old semiotic Co-Forth interpreter is not a supported compatibility language: it remains
temporarily internal to the historical proof/library, stack-console, channel, and peer-demo subsystems
only until useful behavior is ported and the implementation is deleted. It is never a fallback. Core words are now generated through one
immutable signature/documentation/implementation registry, and the broker has a real typed
`(execution_id, sequence)` suspension/resumption boundary with an effect journal. Persisted and
promoted vocabulary still needs the same registry migration. Named Brain storage now restores its
integrity-checked host authority record separately from content-addressed VM checkpoints, so a
checkpoint copied without that record confers no grants. Policy mutation persistence outside Brain
runtime commits is now immediate and fail-closed through an application-owned authority sink.
The persisted authority state now includes an immutable `CapabilityPolicy` identity and
capability-wide denials. Installing a new policy atomically revokes active grants issued under the
prior identity, blocks grants for denied kinds, and is re-read at every host boundary; storage
failure rolls back the policy and its revocations together. Complete host adapters, policy UI, and
provider conformance remain unfinished.

The target removes those explicit migration APIs after conformance parity and gives interpreted
and JIT execution the same verified IR, transaction, and error behavior.

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

### Product boundary: Finch Runtime and Finch application

The design deliberately has two products with one protocol boundary, even while they ship from the
same repository initially:

```text
Finch Runtime
  typed IR + verifier + interpreter/JIT + capabilities + transactions
  + serialized side effects/resumes + diagnostics + durable ProgramRun state

Finch application
  Brain/event log + provider loop + terminal/shadow-buffer UI
  + workspace/OS/automation/MCP adapters + approval policy + scheduling
```

The Runtime is embedder-neutral. It never assumes a terminal, browser, daemon, MCP transport, or
particular model provider; it yields a typed side effect and accepts an idempotently correlated
typed resume. The Finch application is one host implementation of that ABI. It binds effects to
its Brain, environment authority, UI handles, host integrations, and approval policy. Keeping this
line explicit lets an IDE, web client, or another harness execute the same verified Finch program
without reimplementing its language semantics or weakening its capability checks.

The Runtime owns the typed word-registry mechanism, signature/effect validation, and standard
portable event kinds. An embedder owns the concrete bindings it registers: `say`/`output.*` map to
its presentation adapter, `proposal.open` maps to its artifact workflow, and MCP bindings map to
its discovered transports. An application binding cannot smuggle semantics through a description:
it supplies a typed descriptor, capability template, and host handler, all of which the Runtime
verifies before publishing the word in a manifest.

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
closure-converted, and lowered by post-order traversal. Each resulting Lisp instruction currently
carries the exact span of its enclosing top-level source form (including source identity and
line/column coordinates); macro-expanded instructions retain that caller-form provenance. Precise
nested-expression and macro-template expansion chains remain a required source-map refinement,
not a claim that a whole submission is an exact location.

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
stream<T>        scheduler-owned lazy sequence/cursor handle
fiber<Y,R>       deferred producer that may yield Y repeatedly and returns R once
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
dup          forall A S. (S A -- S A A) ! pure
drop         forall A S. (S A -- S) ! pure
+            forall S.   (S int int -- S int) ! pure
file.read    forall R S. (S path<R> -- S bytes) ! {fs.read(R)}
agent.await  forall T S. (S task<T> -- S result<T,agent-error>) ! {agent.await}
yield        forall Y S. (S Y -- S) ! pure ; typed suspension; unit is a timeslice
```

The signature includes:

- input and output stack rows;
- generic type parameters and constraints;
- control-flow behavior such as return, throw, or suspend;
- a capability/effect row;
- optional determinism, allocation, and numeric-overflow properties useful to optimization.

The implementation currently represents type variables in `Type::Variable`, records quantified
names in `StackSignature::type_parameters`, and substitutes a generic word signature against the
caller's concrete stack suffix. That is enough for polymorphic primitives and simple generic calls;
it is not yet a complete source-defined generic/template system. Such definitions must remain in a
parametric elaborated representation while their bodies, constraints, module references, and
source/expansion origins are checked. Lowering may preserve a verified quantified function in IR or
monomorphize a concrete instance when representation or optimization requires it, but the
interpreter-facing module must never guess a template by reparsing source. Lisp and Co-Forth expose
the same facility and lower equivalent instantiations to equivalent stack IR.

### Shared scheduled-execution substrate

CPU tasks, lazy streams, repeatedly-yielding fibers, and detached agents have overlapping
implementation needs: stable IDs, ownership/ancestry, cancellation, budgets, lifecycle state,
ordered event journals, and durable serialization. The daemon therefore owns one internal
scheduled-execution registry. A record binds a stable ID to its verified module or cursor,
environment/Brain identity, grants, budget, status, cancellation state, and terminal result or
diagnostic.

That registry is an implementation substrate, not a promise that these constructs have the same
language semantics. A `task<T>` yields one terminal result, a `stream<T>` exposes a bounded cursor,
a `fiber<Y,R>` exposes producer progress plus a terminal result, and an agent is a separate
ProgramRun with its own authority and provider protocol. No construct shares a parent operand
stack or Rust thread/channel handle merely because it shares lifecycle machinery.

### Fibers, streams, deferred work, and repeated yields

`task<T>` remains the existing opaque scheduler handle. Its await/join operation is terminal: it
returns one final `T`. A lazy `stream<T>` is the simpler multi-value abstraction; it owns a cursor
and advances only when its consumer asks for the next value:

```text
stream-next stream : option<T>  ; bounded pull; none means exhausted
stream-close stream : unit      ; release cursor/cancel its producer
```

`fiber<Y,R>` is the implemented cooperative producer over the same typed `yield` control effect,
exposing a pullable sequence plus a final return:

```text
defer closure       : fiber<Y,R>                  create a pure producer and return immediately
yield value         : unit                        publish one Y and continue when advanced
fiber-next fiber    : result<Y,variant{end(R)}>  ok(Y), or err(end(R)) at terminal return
fiber-join fiber    : R                           discard yields and advance to terminal return
fiber-cancel fiber  : unit                        make later use fail deterministically
```

The source program never writes a continuation. A fiber `yield value` may occur any number of
times; the VM records remaining frames as an internal thunk and advances it through the
runtime-owned producer registry.
This uses the same typed `yield` instruction as ordinary ProgramRuns, not a second fiber-only
primitive: its function/fiber contract declares `Y` and the resume value (initially `unit`), and
the scheduler records both in the same typed suspension record used by every `MaySuspend` word.
Callable signatures and first-class closure types retain this as `yields<Y,unit>` metadata. The
frontends infer it transitively from direct yields and calls, while the independent verifier derives
it again from IR and rejects a function that hides or changes its suspension contract.
If bidirectional generators become necessary, add `fiber<Y,Resume,R>` and give `yield` the typed
stack effect `Y -> Resume`; do not silently use `dynamic` for resumed values. `defer`, `next`, and
`join` are ordinary generated vocabulary bindings over that scheduler record, not syntax-level
exceptions or a privileged multi-return convention. Cursor-backed `stream<T>` remains the simpler
range abstraction; a producer fiber can be adapted to it through visible library code.

Fibers are not the subagent protocol. A subagent is a separate child `ProgramRun`/agent turn with
its own private stack, verified module, capability attenuation, budget, ancestry, event journal,
and durable `task<R>` handle. `agent.spawn`, `agent.poll`, `agent.await`, `agent.cancel`, and later
typed child-message/event operations are the only parent/child communication boundary. A child may
publish progress events to its scheduler-owned task stream, but the parent never resumes a child
through `yield`, receives its continuation, or shares mutable frame/stack state. This keeps agent
streaming, authority auditing, cancellation, and multi-turn orchestration independent from the
language's optional bidirectional-generator feature.

An agent task may be **detached**: its parent stores or returns the `task<R>` handle instead of
awaiting it. The daemon then owns the child across provider calls, timer/I/O waits, approvals, and
user input, publishing progress and a terminal result as ordered Brain events. This is autonomous
long-running orchestration, not a periodic scheduled task: a timer is merely one awaitable event in
the child run. A later user or program turn can poll, join, cancel, or send a typed message to the
handle subject to ancestry and capability checks.

A detached child never gains new authority while nobody is present to approve it. At detach time it
receives only the explicitly attenuated grants, grant lifetimes, module hash, expected result type,
budget, and ancestry recorded in its durable task record. A request outside that set enters a
`pending-approval` state with one coalesced notification to an eligible owner; it neither retries
nor widens itself. Policy chooses an explicit bounded expiry: on expiry it fails with an auditable
`ApprovalUnavailable` result, or a human/daemon explicitly resumes it after grant. A disconnected
frontend therefore cannot turn a parked host-machine request into unattended machine control.

CPU-bound work has a more direct source form and is not an agent or a generator. Initially Lisp
uses `(defer :cpu (lambda () ...))`; Co-Forth lowers the equivalent quotation through
`defer-cpu`. It captures immutable typed values, starts with a private stack, and returns a
`task<T>` whose `poll`, `join`, and `cancel` operations are terminal task operations. The scheduler
may use OS worker threads for these tasks, but neither thread handles nor parent stacks are VM
values. I/O waits and timer waits suspend a ProgramRun through the trampoline instead.

Definitions may declare signatures, but the compiler derives and validates them. Inferred public
signatures are stored in the vocabulary manifest. Unresolved calls, stack-dependent parsing, or
unsafe reflection prevent proof and require an explicit dynamic/unsafe boundary.

### No privileged collection or iteration overloads

Surface convenience must never create a standard-library-only fast path. A future `for`/`foreach`
form may be compiler-owned syntax that selects an indexed loop, range loop, fiber pull loop, or
collection-specific loop during lowering. Each selection must be justified by a public structural
contract, conceptually `empty?`, `front`, and `pop-front` (or the equivalent `next` contract). A
user-defined range supplies the same visible typed words and resolves to the same concrete word IDs
as a built-in range. The optimizer may inline, specialize, fuse, or eliminate allocations after
that resolution, but it may not recognize only `list`, `map`, or a compiler-owned iterator type
while treating an equivalent user definition as dynamic dispatch. A user-written `foreach`,
traversal, or adapter must remain eligible for the same optimizations as syntax supplied by Finch.
There is one staged `foreach`, not a separate `static foreach`: when its range and pure body are
compile-time values, bounded CTFE executes it; when the range is a runtime value, lowering emits the
ordinary verified range loop. Partial evaluation may specialize known structure and leave residual
runtime code, using the same public contracts in either stage.

The exception is the deliberately small execution substrate: verified branch/suspend instructions,
managed allocation, and authorized host calls. Those are represented by public typed words and
their contracts in the registry; user source cannot manufacture arbitrary IR or host authority.
Everything above that substrate—including collection algorithms and range iteration—remains
ordinary vocabulary that can be inspected, replaced, composed, and optimized.

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

Network connections follow the same rule. `network.connect` instantiates a concrete host/port
requirement from typed arguments and returns an opaque host-issued socket resource. A later send
does not gain ambient network authority from that resource: the host retains the socket endpoint
and rechecks it against the active grants on every operation, including after revocation or a
resume. Source code cannot manufacture a socket handle or substitute a different endpoint.

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

### MCP adapters are host bindings, not a second VM

Finch's MCP implementation is a client-side integration mechanism. It does not become the VM's
subagent protocol, continuation protocol, or an untyped escape hatch. Server configuration,
transport lifecycle, discovery, refresh, and trust of a stdio/SSE process remain host-owned. A
connected server's discovered tools are converted into versioned, namespaced vocabulary bindings
only after their JSON Schema is validated.

Each admitted binding is generated from one descriptor containing its qualified name (for example
`mcp.github.issue_get`), schema-derived input/output types or an explicit managed `json` boundary,
documentation, capability requirement, selector template, availability state, and host handler.
Calling it lowers to the normal typed host-request event and therefore follows the normal
grant/approval/suspension/resume/audit path. The result is schema-checked before it re-enters the
VM. An arbitrary MCP schema must never silently become `dynamic` values on the stack; unsupported
schemas use the explicit `json` boundary or are not published.

MCP names, descriptions, annotations, and examples are third-party untrusted data. They may be
shown as quoted metadata to a user or provider, but never treated as Finch instructions, policy,
capabilities, prompt text with authority, or documentation that overrides the BOOT capsule. Bound
their length, preserve provenance, and escape/render them as data in every manifest and UI.

MCP authority is distinct from the process authority used to start a local stdio server. A call
requires an attenuable request such as `mcp.call(server="github", tool="issue_get", repo=...)`;
the host can grant one server, one tool, or a bounded argument selector without granting all MCP
tools. Provider manifests include only relevant, currently available bindings, with normal
introspection for the remainder. This lets a repaired MCP client feed the common registry without
duplicating Finch's authorization or agent orchestration logic.

## Typed Co-Forth language definition

The exact surface grammar will be frozen through an RFC, but the language contract must include the
following constructs.

### Definitions and signatures

Illustrative syntax:

```forth
: square ( S int -- S int ! pure )
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
[ int -- int ! pure | 1 + ]
```

An escaping quotation is closure-converted into an immutable code reference plus a managed captured
environment. Calls use `call`/`tail-call`; they do not create an untyped anonymous stack.

### Closure conversion and capture ownership

Closure conversion is a concrete lowering pass, not a second evaluator. The frontend resolves each
free lexical name to the nearest immutable binding, orders captures deterministically by resolved
binding identity, emits the capture loads in that order, and emits `MakeClosure(function,
capture_count, signature)`. The generated function has a typed capture vector and reads it only
through `CaptureGet`; parameters become frame locals in normal call order. A closure therefore
captures values, never an alias to a caller operand stack, mutable frame, grant, or ambient host
authority.

For example, this Lisp:

```lisp
(let ((n 10))
  ((lambda ((x : int)) (+ x n)) 5))
```

lowers conceptually to the following one shared IR module (the concrete block ids and source
origins are omitted here):

```text
main:
  const.int 10
  make-closure lambda$0 captures=1 : (S int -- S int ! pure)
  const.int 5
  call-closure (S int -- S int ! pure)
  return

lambda$0 captures: [int], locals: [int] # n is capture[0], x is local[0]
  local.set 0                 # pop x from the callee's private operand window
  local.get 0
  capture.get 0
  call core.add
  return
```

The runtime consumes the closure for `CallClosure`, creates a fresh frame with a private operand
window above the caller boundary, copies the immutable captures into that frame, and destroys the
frame on return. Only the signature-declared results cross back into the caller window. This is
also why `(defer :cpu (lambda () ...))` is safe: it serializes/snapshots the closure's immutable
captures into a separate CPU task and never shares the parent stack.

**Initial representation and allocation rule.** Primitive captures (`int`, `bool`, `float`,
`char`, symbols, small opaque handles) are copied inline in the closure value. Immutable structural
values may be reference-counted/managed handles; copying the closure copies the handle, not a
mutable payload. The initial interpreter may represent a short-lived closure as an owned
`TypedValue::Closure` and needs no tracing heap. It must not manufacture a heap environment for a
non-escaping direct call merely for frontend convenience. Later escape analysis may stack-allocate
or inline a closure that is immediately called and never stored, returned, deferred, or passed to
an unknown callee; that optimization is semantics-preserving and optional. A closure is treated as
escaping, and its environment gets stable managed ownership, when it is returned, stored in a
collection/record/dictionary, placed on the persistent VM stack, passed through `dynamic`, used by
`defer`, or handed to a host boundary.

Capability requirements compose from the generated function signature into `MakeClosure` and every
call site. Capturing a string/path/resource does not capture authority; only the resulting verified
call's inferred effect row can request a grant. Tests must cover capture ordering, lexical shadowing,
direct invocation, escaping/persistent closure values, CPU-deferred capture snapshots, and an
effectful closure that suspends before committing its parent transaction.

### Control flow

`if/else/then`, loops, pattern matching, early returns, and exception/result operations must have
explicit IR blocks. Every merge point requires compatible typed stacks. Loops require a stable
stack invariant. Arbitrary jumps are not part of verified source.

The initial named-loop form is implemented without arbitrary jumps: Lisp spells a label as
`(while :label label condition body...)` and uses `(break label)` / `(continue label)`; Co-Forth
uses `begin: label` with `break label` / `continue label`. Each exit must preserve exactly the
target loop's header stack row. Lisp `match` and integer Co-Forth `case` (with no C-style
fallthrough) now lower to verified branch edges. The next control-flow extension is
expression-valued named breaks, where a break target declares its
result stack row and every reachable break must produce exactly that row. This permits nested-loop
exits and useful expression-valued loops without allowing a branch to strand intermediate values
on a caller stack. `for` may be added only as a bounded desugaring to these loop blocks. `try` handles
typed `result`/diagnostic values; it is not an ambient exception escape hatch around effects.

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

The intended experience is *statically safe scripting*, not annotation-heavy systems programming.
Infer literals, locals, parameters, results, stack rows, effects, yields, and generic
instantiations whenever the program determines them. Private Lisp and Co-Forth definitions should
normally need no annotations; publication freezes an inferred or explicitly declared stable
signature. Require annotations at genuinely ambiguous or recursive module interfaces, refinement
and capability-selector boundaries, and FFI—not merely because the compiler implementation has
not yet propagated information. Concepts, parameter packs, ranges, overload resolution, and
bounded CTFE should make routine code feel as direct as Python or JavaScript while retaining one
static, optimizable execution path. Do not achieve convenience by silently inserting `dynamic`,
unchecked coercions, or an interpreter-only fallback.

Inference is deliberately directional rather than global Hindley-Milner constraint solving. An
initializer or literal establishes a local binding's type; subsequent calls check that known type
against their parameter contracts. For example, `let foo = 3; bar(foo)` with `bar : string -> ...`
must diagnose the argument at `bar(foo)`, not infer `foo` backward as `string` and blame `3`.
Generic type/value arguments are inferred forward from the supplied arguments into one bounded
specialization. Expected result types may select among already-valid results but must not rewrite
earlier bindings or cause distant diagnostic locations. This keeps inference incremental, fast,
and explainable to both humans and models.

### Lowering

The frontend performs:

1. parse with exact source spans;
2. hygienic macro expansion in a restricted compile-time environment;
3. name resolution and lexical binding;
4. directional local inference plus explicit effect rows and practical subtyping/refinement checks;
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

Do not create a privileged second macro language. A macro is an ordinary pure, bounded Finch CTFE
function whose contract is `syntax -> syntax` (or a richer typed syntax/context/result record when
needed). The same staged evaluator used for compile-time `if`, `foreach`, generics, concepts, and
derivation executes it. Convenient declarations such as `define-syntax` may remain reader sugar for
defining/registering such a function, but may not acquire separate evaluation semantics.

Syntax values are not bare lists. They retain source origin, expansion ancestry, lexical scope
marks, and stable module/symbol identity. Public syntax constructors and projections preserve those
properties so ordinary structural Finch code can be hygienic without receiving ambient host access.
Macro execution has explicit fuel, recursion, and allocation limits. Expansion provenance maps
generated forms back to both macro invocation and macro definition. A macro cannot hide effects:
the expanded IR is what the verifier analyzes.

Classic S-expressions remain one exact, canonical structural reader, not a requirement that every
human-facing Lisp spelling pay the full parenthesis cost. Later expression/indentation/call sugar
may provide forms such as `foo(a, b + c)`, but the reader must convert each convenience spelling
immediately into the same syntax tree before expansion or semantic analysis. Sugar never adds a
second semantic construct, staging rule, or compiler lowering path. The property to preserve is
syntax-as-ordinary-data, not a mandate that every surface syntax look homoiconic. Conformance tests
must pair every convenience spelling with its canonical S-expression and prove structural syntax
equivalence after ignoring spelling-specific source origins, followed by identical elaborated
HIR/IR. This is reader notation, not a third `FinchScript` language or frontend.

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

### Trampolined execution and resumable waits

The VM uses an internal continuation protocol; it does **not** implement general user-visible
`call/cc`. A running program is represented by a typed frame stack (function/module identity,
instruction position, locals/captures, data stack, effect journal, fuel, and source trace). Stepping
that state returns exactly one of:

```text
Continue(thunk)                 execute the next bounded VM slice
Emit(event, thunk)              publish one structured side-effect event, then continue
Await(request, resume_thunk)    persist/schedule the request; do not block a VM or UI thread
Complete(values, journal)       commit the transaction
Fail(diagnostic)                discard uncommitted VM-local mutation
```

`thunk` is the runtime's implementation term for a zero-argument continuation. In memory it may
be a compact frame object; for a durable Brain it must serialize as VM data rather than an opaque
Rust closure. The event loop is the trampoline: it repeatedly invokes `Continue`/`Emit` thunks,
projects emitted events to the shadow buffer, and stores `Await` continuations. The current
interactive provider-wire runner automatically requeues only a unit-valued `yield` after first
yielding its Tokio task; approval, timer, agent-completion, and host-I/O events require their
explicit host lifecycle before they resume the saved thunk with a typed result. They never
resubmit source text or mutate an LLM prompt.

This is also the streaming rule. `say` yields an `Emit(ResponseChunk(...), thunk)` event; it does
not write the terminal directly. A program can therefore emit text, compute more values, emit
again, and only later complete or await. The renderer owns coalescing/replay while the complete
event stream remains testable and recoverable after reconnect.

### Portable side-effect protocol and reactive output handles

The VM is deliberately independent of Finch's terminal UI, providers, and host integrations. Its
serialized execution protocol is the contract another harness can adopt:

```text
VmSideEffect {
  protocol_version, sequence,
  kind, typed_arguments, expected_output_row,
  capability_requirement?, source_origin
}
VmResume { execution_id, sequence, typed_result | denial | cancellation }
```

`execution_id` belongs to the enclosing `ProgramRun`/transport envelope; together with `sequence`
it is the idempotency key. The VM records an effect in its journal and yields its serialized continuation. It does **not** call
a terminal, `OutputManager`, filesystem, browser, provider, or scheduler. A harness can render the
event, queue it, reject it, execute it remotely, replay its already-recorded result, and resume the
same continuation idempotently. `VmResume` is accepted only for the awaiting `(execution_id,
sequence)` pair; its typed result must match the verifier-known output row, is recorded as the
acknowledgement, and must not redispatch the host effect. `effect_id` and the journal make
at-least-once transport safe while the host-effect adapter supplies exactly-once host execution.
The per-run observer receives an awaited event when it enters the journal, before a local binding
or approval decision, so an external event loop can own its presentation and later return the
correlated result.
The reference runtime offers both a compatibility policy (only editor proposals suspend) and a
portable-host policy (every approved awaited capability suspends). The latter is the actual
embedder seam: files, processes, network calls, and UI handle creation can be implemented by an
IDE, web host, or daemon and returned through the same `VmResume` record rather than through a
Finch-specific synchronous callback.

UI output is a first-class family of these events, not an overloaded string channel. There is no
global “active WorkUnit”: a `ProgramRun` receives a host-owned **default response port** when the
interface submits it. `say` appends durable response text to that particular port, and the binding
travels with a saved suspension. Therefore a download, a provider turn, and an autonomous task can
remain visible and update independently. The UI event loop, not the VM, owns the map from a stable
`(execution_id, output_handle)` to a shadow-buffer object.

Explicit operations create or mutate host-issued typed output handles: append a response fragment,
replace a handle's formatted content, append a live tool/log row, set transient working/progress
state, complete, or fail it. Handles are opaque resources; their formatter and lifecycle remain
host-owned. Finch maps the default response port to its existing `OutputManager`/`WorkUnit` message
handles, whose shadow-buffer renderer can update an in-progress message repeatedly before committing
it exactly once to terminal scrollback. Another harness may map the same events to a web DOM, IDE
panel, voice UI, or an audit log without changing VM code.

The initial portable output vocabulary is intentionally small: `say` appends to the run's default
response port; explicit host-issued output resources support `output.append`, `output.replace`,
`output.status`, `output.progress`, `output.complete`, and `output.fail`. These names describe
event semantics, not terminal escape sequences or a global current work item. `output.progress`
contains a bounded current/total or indeterminate state, so a download and a response can update
concurrently. The host validates an output handle's ownership and generation before projecting an
event. A handler that cannot render a richer operation preserves it in the journal rather than
silently collapsing it into text.

Finch's terminal host projects these events through a per-tool presentation binding: ordinary
`say` appends to the generation response `WorkUnit`, while `output-open` creates an independent
shadow-buffer `WorkUnit` keyed by its opaque handle. `append`, `replace`, `status`, `progress`,
`complete`, and `fail` update that exact unit. This adapter belongs entirely to the application;
an IDE, web client, or accessibility host can project the same event stream differently.

Output resources are owned by the ProgramRun that opened them. They remain valid across that
run's serialized yield or approval resumption, but a completed, failed, stale-generation, or
different ProgramRun cannot update them. This is a host-validated resource boundary, not a
convention for choosing an ambient work unit.

Program proposals are another explicit host/UI effect, not an implicit consequence of Forth. A
typed `proposal.open` capability creates an editable proposal handle for a Finch, Bash, Python, or
other source artifact; the harness can open `$EDITOR`, show the shadow-buffer proposal surface,
accept co-edits, request ordinary capability approval, or cancel. Simple typed operations run
through their normal capability grants without opening an editor, so an agent does not produce one
proposal dialog per command. A proposal can itself contain a Finch Lisp/Co-Forth program and is
evaluated under the same verifier and broker after acceptance.

The proposal lifecycle is a separate durable state machine, not a synchronous “editor call that
executes a string”:

```text
proposal.open
  -> proposal.created(handle, language, source_hash, generation=0)
  -> proposal.awaiting_edit(handle, generation)
  -> proposal.accepted(handle, generation, source_hash)
   | proposal.chat(handle, generation, context)
   | proposal.cancelled(handle, generation)

proposal.submit(handle, expected_generation)
  -> new verified Finch ProgramRun
   | separately authorized external-script execution request
```

At the portable Runtime boundary, the pending `(execution_id, sequence)` effect is the stable
correlation key while the proposal is awaiting a host result. A host with an event-loop binding
does not block the VM runner in `$EDITOR`: after the `program.invoke(language=...)` grant, it
projects the request, records its own `created → awaiting-edit → …` application events, and resumes
the exact verified continuation with an accepted/chat/cancel value. The legacy synchronous editor
adapter is only a compatibility projection for hosts that have not adopted this lifecycle yet.
Runtime callback adapters receive this key in a `VmEffectEnvelope { execution_id, effect }` and
may persist its named `VmEffectHandle { execution_id, sequence }`; the portable `VmSideEffect`
remains independently serializable for other embedders.

Finch's frontend controller treats a suspended `program.invoke` outcome as an unfinished tool
call. It opens the language-aware editor on a separate frontend task, maps the editor directive to
the verified option/result output row, resumes `VmEffectHandle`, and only then completes the
original tool call to the provider. Thus a model never receives an intermediate “editor opened”
result and cannot accidentally continue from stale proposal source. Durable Brain-journal replay
of the presentation transitions remains a separate integration step.

`proposal.open` and editor changes never execute source. `proposal.submit` is an explicit,
idempotent `(handle, generation)` action after acceptance; it creates a new ProgramRun rather
than resuming the opener’s VM continuation, so it cannot replay prior effects or inherit accidental
stack state. A stale edit or submit fails with a structured generation diagnostic. The event journal
records each transition, allowing reconnection to re-render an existing proposal without reopening
an editor or re-running an external effect. Proposal language is a parameterized capability
selector, so a grant for Python artifacts cannot open Bash or Finch artifacts.

This does **not** require a separate proposal database: the durable Brain/event journal is the
authoritative proposal record, and an editor or shadow-buffer client is only a projection of that
record. A temporary editor file is an implementation detail, never the source of truth.

Phase 0 is deliberately useful without model-authored control flow: project the existing provider
stream into this same event journal and handle lifecycle, then test replay/reconnect and concurrent
presentation bindings against real traffic. A report-only corpus replay of existing model-emitted
Co-Forth follows it, classifying typed-verifier rejections before typed mode becomes mandatory.

The current typed host handler is only a compatibility adapter over this boundary. It must be
replaced progressively by the portable event journal/resume interface rather than becoming a
second VM execution path.

### Brain stack ownership and first-class task handles

A user message is a Brain-turn event, model inference is a provider job, and a submitted Lisp or
Co-Forth artifact becomes a `ProgramRun`; none of those objects is implicitly a mutable VM stack.
One Brain owns the authoritative persistent typed stack/dictionary/heap revision. A ProgramRun
starts from that revision, evaluates against a private transactional working state, and commits a
delta only if its expected revision still matches.

Deferred CPU fibers and child agents never receive the parent stack as shared mutable memory. They
receive explicit typed arguments or immutable captured values and own private stacks. They return
typed results/events through a daemon-owned handle; `join` resumes the parent run and places the
returned value on its private working stack before it commits. This preserves no-GIL concurrency
without turning positional Forth stack state into a data race.

`fiber<Y,R>` and `task<R>` are first-class persistent values: their serialized form is a stable
daemon task ID plus Brain/environment identity, owner/ancestry, expected types, creation revision,
budget, and policy reference—not a Rust channel, OS thread handle, or child stack. A later program
may keep such a handle on the persistent Brain stack, inspect/poll it, consume yielded values, join
its terminal result, or cancel it subject to ownership and capability checks. The daemon owns the
worker state, event queue, result/error, and cancellation record.

The continuation is bound to the verified module hash, VM revision/checkpoint, capability-grant
reference, budget, ancestry, and pending request ID. The first implementation now serializes the
verified module, explicit frames, typed stack, fuel, and pending typed host call. The daemon keeps
the resulting suspension under the UI execution ID, validates manifest and VM revision, and resumes
that exact frame after a grant. Brain checkpoint persistence, grant-reference/ancestry validation,
and pending request IDs remain the remaining integration work. Resumption must verify those
bindings before executing. This prevents an approval, child result, or scheduled event from being
replayed against a different program or state revision.

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

### Model-authored vocabulary evolution

Executable knowledge is a first-class product surface, separate from model-weight adaptation and
MemTree/context adaptation. A model may discover a reusable procedure and define it using the same
typed vocabulary available to a human:

```forth
: investigate-regression  repo diff affected-tests run-tests summarize ;
```

Definitions have explicit lifetimes and promotion boundaries:

```text
ephemeral → task → session → project → user → published package
```

An ephemeral or task-local definition may be created in the execution transaction. Promotion to a
broader dictionary is an authority-bearing operation, not an incidental side effect. For example,
`project.publish` requires `{vm.write(dictionary="project")}` and a published package additionally
requires provenance, dependency versions, signature/effect certificates, tests, and review state.
Each promotion creates an immutable word version; existing callers continue to reference their
original version. Revocation removes the promoted name from future manifests without invalidating
already-audited historical executions. Providers discover the relevant vocabulary manifest rather
than receiving arbitrary model-authored words implicitly.

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

Resource roots, rather than raw path strings, define the spatial boundary. The usual `path<R>` is
relative to an immutable workspace/project root and rejects traversal or symlink escape at the
call boundary. A user may deliberately grant whole-machine control, but that creates a distinct
host-issued root resource (for example `root<host-machine>`) with a broad, auditable capability;
it does not make arbitrary absolute strings ambient authority. The same type and selector rules
then apply below that root. This preserves both the autonomous-workspace fast path and an
intentional full-control mode without confusing either with a glob heuristic.

Persisted decisions store a typed selector, root identity, operation, scope, source/policy binding,
creator, timestamp, and revocation state. They never store only the display string. The UI provides
searchable history and immediate revocation.

Application-owned authority persistence is independent from VM transaction commits. A named Brain
installs a sink that atomically replaces its separate integrity-checked authority record whenever a
grant, revocation, denial, or host-authorization audit mutates the ledger. The mutation becomes
visible in memory only if persistence succeeds; a sink failure restores the previous ledger and the
host operation fails closed. Consequently an external effect remains audited even when its
ProgramRun later rolls back, while a failed VM transaction cannot erase or manufacture authority.
Archiving a Brain detaches this sink before removing the live runtime so a retained runtime handle
cannot recreate the archived policy path.

The authority record also owns the current immutable `CapabilityPolicy`. Its identity binds every
grant to the policy revision under which it was approved. Installing a different identity revokes
all still-active grants from the former revision in the same persisted mutation; capability-wide
denials prevent reissuance, and intrinsic `session_emit`/`vm_read` operations cannot be disabled by
host policy. Reusing an identity for different contents is rejected. Execution and resumption
derive compact grants from the current identity, while the host call boundary independently reads
the live policy and ledger again. This closes the race in which a ProgramRun began before a policy
change but reached an external operation afterward. Pre-policy integrity-signed authority files
are verified against their exact historical payload before receiving the original default policy.

Scheduled callbacks use the same broker rather than a parallel queue authority path. Creation
returns an opaque host-issued `schedule` resource; `schedule-get` requires `schedule_read` and
returns redacted managed JSON without the callback's persisted authority ceiling, while
`schedule-cancel` requires `schedule_manage` and retains a cancelled durable record. The queue
atomically changes `Pending` to either `Running` or `Cancelled`, so a cancellation cannot succeed
after a scheduler has claimed the callback and two runners cannot execute the same pending row.

The VM's compact active `EffectSet` is only a fast execution guard. Immediately before an
authorized non-intrinsic effect is dispatched locally or handed to a portable host, the Finch host
reconstructs its deterministic request identity from `(execution_id, effect sequence)`, resolves it
against the scoped ledger, and records the stable grant ID. A deferred host result refers back to
that same authorization fact rather than consuming or auditing the grant a second time. If the
ledger no longer supplies a matching active grant before dispatch, the host boundary fails closed
even if a stale private runtime snapshot still contains the broader compact effect set.

### Transaction rule

A persistent VM execution builds a delta. It commits stack/dictionary/heap changes only when:

- verification succeeded;
- all required synchronous capabilities completed successfully;
- no uncaught error or cancellation occurred;
- its expected VM revision still matches or its delta merges without conflict.

External effects are journaled execute-once facts and are never claimed to roll back with VM state.
Suspension preserves a typed continuation and transaction; resumption rechecks environment,
manifest, grant, program, and resource generations before continuing.

Every effect journal entry has one of the explicit states `proposed`, `awaiting_approval`,
`acknowledged(result)`, `denied`, `cancelled`, or `failed(diagnostic)`. A host performs an external
mutation only after the entry has a stable `(execution_id, sequence)` idempotency key; it records
the acknowledgement before allowing the VM to advance. If a later instruction fails, the outcome
contains both the VM rollback and the acknowledged-effect prefix. A host-binding failure is itself
preserved as a journal state, since an adapter may have produced a partial external effect before
it could return a resume value. Finch must never claim atomic success or silently retry that
prefix. A future `commit-effects`/barrier form may make this boundary visible to source code;
reversible operations require an explicitly typed compensator, not a rollback illusion.

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

### Performance and expressiveness targets

Native-code generation is not itself the performance goal. For monomorphized effect-free numeric,
collection, parsing, and control-flow kernels, optimized Finch should compete in the performance
class of optimized Rust/C++ on the same machine. Track wall time, allocations, peak resident memory,
code size, compile latency, and dispatch/host-boundary overhead against checked-in equivalent Rust,
C++, and interpreter baselines. Publish distributions over representative programs; do not claim
parity from one arithmetic microbenchmark. Gaps caused by missing vectorization, alias analysis,
escape analysis, bounds-check elimination, or range fusion remain named compiler work.

The source-language expressiveness target is comparable to TypeScript for ordinary application
modeling—structural records, closed variants, closures, parametric functions, structural concepts,
modules, reflection/derivation, asynchronous resources, and ergonomic collection/range composition—
without JavaScript prototype mutation or `dynamic` as the routine escape hatch. Both Lisp and
Co-Forth must expose that same typed semantic surface even when Lisp is the more ergonomic human
frontend. Maintain a corpus of equivalent application-sized programs to measure source size,
required annotations, diagnostic quality, first-pass model success, incremental compile latency,
and generated IR as features land. Annotation density is a product metric: common private
application code should read like a scripting language even though module publication and the
independent verifier retain complete static signatures.

### Eventual self-hosting

Once retained syntax/HIR, the semantic scheduler, CTFE, modules, and AOT ABI are stable, the
frontend and most semantic jobs may themselves be ordinary bounded Finch programs. Rust and other
embedders call that compiler through a small versioned typed service interface; an AOT build may
also export a C-compatible `libfinch_compiler` facade generated from the same declarations used by
ordinary FFI. Keep the reader framing, artifact loader, verifier, effect boundary, and minimal
runtime as a deliberately small stage-0 trusted implementation rather than requiring an existing
self-hosted compiler to validate arbitrary input.

Bootstrap reproducibility is mandatory. Check in a content-addressed verified compiler artifact,
use stage 0 to compile the Finch compiler source into stage 1, use stage 1 to produce stage 2, and
require normalized stage-1/stage-2 IR or native artifacts to agree. Record compiler source, module
graph, runtime ABI, target, and dependency hashes. Self-hosting must not introduce a privileged AST,
type, CTFE, or code-generation path unavailable to the inspectable language modules it exercises.

### Later AOT compiler target

After the interpreter contract and JIT differential gates are stable, the same verified pipeline
may expose a separate `finchc` target. Pure programs may link a minimal runtime and produce ordinary
standalone executables. Programs with host effects instead link the portable
`VmSideEffect`/`VmResume` ABI and require a capability-providing embedder. Both modes consume the
same span-preserving AST/parametric HIR, dependency scheduler, CTFE/monomorphization cache, verified
stack IR, and source maps; there is no AOT-only source language or trusted model-authored CLIF.
Host selection is explicit. A `none` profile rejects any inferred effect it cannot satisfy; a small
terminal wrapper may project `session.emit` to stdout/stderr and implement a declared bounded host
surface; portable or object/library output exposes or leaves unresolved the effect/resume shims for
an embedder. The executable carries its inferred effect manifest. `say` always means the same
`session.emit` effect—it never silently becomes a distinct native-print operation.

A standard asynchronous application host is a third ordinary profile, not a language exception.
It links a small Finch runtime that owns the platform poller and maps opaque, generation-checked
listener/socket/file resources to native descriptors. Typed `connect`, `listen`, `accept`, `read`,
`write`, and `close` operations suspend and resume through the same effect ABI used by an
interactive embedder. HTTP clients and servers can then be Finch libraries over byte streams (with
optional optimized host vocabulary), while source programs never receive a forgeable integer file
descriptor. Code that intentionally needs raw descriptor or foreign-ABI manipulation must enter an
explicit unsafe native-extension boundary with a separately declared capability; producing an AOT
binary does not silently grant that authority.

The asynchronous host is selected through a narrow reactor/scheduler interface rather than being
hard-wired to Tokio or one operating-system poller. A standalone service may let the Finch runtime
own the loop; a Cocoa, Win32, GTK, game, or existing C application may instead own the main thread
and supply timers, readiness registration, wakeups, and event delivery. Native callbacks enqueue a
typed correlated resumption onto that scheduler and do not re-enter arbitrary VM frames directly.
This keeps continuation ordering, cancellation, and execute-once effect records intact when the
host loop is swapped.

Later C interoperability should use versioned typed `extern` declarations and generated ABI shims.
Safe wrappers describe argument/result layout, ownership, callback lifetime, thread affinity, and
effects; opaque C pointers remain generation-checked resources. Calling an unverified symbol,
passing a raw pointer/integer descriptor, variadic calls, and unchecked shared-memory access require
an explicit unsafe-FFI capability. The same declarations feed interpreter bindings and Cranelift
AOT lowering so FFI does not become a second language semantic path.

Compile-time reflection should make immutable `type`, schema, syntax, symbol/module-reference, and
constraint-evidence values available to pure bounded Finch functions. Generics, concepts,
compile-time branching/traversal, derive operations, and hygienic macros must all use this one staged
evaluation model. Generated definitions are structured syntax with expansion provenance and are
verified normally; string mixins and overlapping special-purpose metaprogramming subsystems are not
part of the design.

Type-safe variadics are a required consequence of that general template model, not a privileged
calling convention. A generic definition may bind a type/value parameter pack, inspect its length,
index or destructure it, and traverse its ordered type/value pairs with ordinary bounded compile-time
`foreach`. Instantiation produces an ordinary concrete fixed-arity signature (or deliberately lowers
a homogeneous pack to a typed list/range), so verification, specialization, inlining, and effect
inference see every argument. Both Lisp and Co-Forth must be able to define and consume the same pack
abstraction; built-ins do not receive a variadic facility unavailable to user code. This is distinct
from C ABI `...`: an `extern` declaration for untyped `va_list`/raw variadics remains an explicitly
unsafe FFI boundary, while a typed wrapper may use CTFE packs to present a safe Finch interface.

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
- Keep native Lisp execution removed; new semantics must lower to shared typed IR.
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
8. Keep the removed native Lisp evaluator from returning as a compatibility escape hatch; missing
   closure, macro, capability, persistence, or diagnostic semantics must be implemented in shared IR.

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
