# Finch Language Design

## Status and relationship to existing plans

This document specifies the intended design of one typed Finch language runtime with two initial
source syntaxes. It deliberately describes future semantics so early implementation choices do not
make the coherent version prohibitively expensive. Implementation order, current gaps, crate
transitions, and deletion gates live in the separate
[language implementation roadmap](IMPLEMENTATION_ROADMAP.md).

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

The canonical submission envelope carries an explicit `language: "forth" | "lisp"` field; that
metadata defines language identity for storage, tools, IPC, and non-provider transports. A compact
streaming model response that deliberately omits the envelope may use a cheap shorthand: first
non-whitespace byte `(` selects Finch Lisp and every other response selects Co-Forth. This shorthand
never overrides an explicit tag and is not part of either language's source identity. The dispatcher
does not guess from prose and the provider prompt says that user-visible prose must be emitted by a
VM operation such as `"hello" say`, never written outside the program. Empty responses and Markdown
fences are explicit malformed-wire diagnostics with corrective guidance, not accidental Co-Forth
words. Co-Forth is therefore the natural streaming form: the receiver can parse and render complete
tokens while waiting for later tokens, whereas Lisp remains preferable when nested structure makes
the leading `(` worth it.

Bare `"text"` is the preferred short escaped-string literal in Co-Forth. `s"text"` remains a
Forth-compatible equivalent and has no implicit leading space: both `s"text"` and conventional
`s" text"` produce `text` because the one delimiter whitespace is consumed. Spacing belongs in
the literal or is composed explicitly (for example `space`, `str-cat`, or separate `say` events).
`"""..."""` is the preferred short raw multiline/prose literal; compatible `s"""..."""` also
works. It preserves contents until the next triple quote. Content containing that terminator uses
the selectable raw fence `r#"..."#` (with additional matching hashes as needed), so every valid
source-text payload remains expressible without escaping or another string type. Co-Forth uses `\\`
line comments. In an untagged compact provider stream,
parenthesized Co-Forth comments are allowed only after a Co-Forth token because leading `(` is the
Lisp shorthand. Explicitly tagged Co-Forth has no such transport ambiguity and may begin with a
parenthesized comment. The normative language definition must give exact escaping and raw-delimiter
examples.

Finch scripts are portable, self-contained source artifacts rather than shell wrappers. A script
may carry an explicit language in its shebang/launcher metadata, for example
`#!/usr/bin/env -S finch --exec --language=lisp`; the launcher removes the shebang before the chosen
frontend receives source. `finch --exec` selects the strict typed runtime and must never silently
fall back to legacy interpreters. A script without language metadata must be supplied an explicit
CLI/tool language rather than borrowing the compact provider-stream shorthand. Imports and
namespaces are a later package feature: model-emitted one-off scripts should normally be complete
and auditable in one file. Bash, Python, and other external scripts remain valid *proposal
artifacts* when they are the appropriate user-editable delivery format; Finch scripts do not
remove that capability.

### One parse boundary, modules, and packages

Each Lisp or Co-Forth module's source bytes cross one authoritative parse boundary into an explicit
span-bearing frontend AST. This is a representation and pipeline constraint, not a requirement to
stream lexer tokens directly into IR or to forbid bounded lookahead, token buffering, or declaration
indexing inside the parser. After that boundary, macro expansion, name resolution, type inference,
optimization, linking, and independent IR verification operate on retained structured data; none
may rescan source, serialize transformed code, and reparse it. The independent verifier remains
mandatory because it proves the produced IR rather than interpreting source a second time.

Each source identity owns one immutable byte buffer, normally interned by content identity for the
life of every diagnostic/compiler artifact that refers to it. Original-source spans are compact
`(source_id, start_byte, end_byte)` references into that buffer; AST nodes do not allocate substring
copies merely to retain spelling or location. A reader may intern a decoded symbol or store the
semantic value of a literal, but diagnostics, source maps, and macro provenance recover original
text lazily through the span. Offsets are byte offsets with validated token boundaries, so slicing
never depends on platform character width. Generated syntax carries an expansion-origin chain and
only owns text that has no original source span. Serialization deduplicates the source table rather
than embedding the same source fragment in each node.

Source order is independent from this parsing constraint. During parsing, the frontend registers a
top-level declaration skeleton as soon as its header is stable and publishes its body node when that
region is complete. A job may then wait for a skeleton encountered later, so later definitions are
valid forward references. The dependency scheduler resolves them on demand.
Explicit signatures are required for exported module interfaces, genuinely ambiguous inference,
or cycles that cannot otherwise reach `SignatureReady`—not merely because a callee appears later
in the file. Macros are bounded structured transformations, not context-sensitive token
reinterpretation. A parser never guesses and revisits an earlier token after discovering a later
declaration; the retained AST and symbol registry carry that information into semantic analysis.

The parse-once rule does not mean emitting final IR directly from lexer tokens. Each frontend must
produce an explicit, span-preserving syntax tree. Lisp already has the
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
operate on those nodes, and one semantic lowering stage emits the common typed stack IR in
evaluation order. A cached parametric body may be instantiated or lowered more than once when its
layout or specialization requires it; “one lowering stage” does not require one physical visit to
each source node.
Generated syntax retains both its call-site and definition origin and is never converted to text
and reparsed.

Lisp and Co-Forth are the first two frontends, not a closed set. A future language frontend may
parse its own source once and submit span-bearing structured syntax through a versioned semantic-
construction protocol. That protocol is a typed builder surface—operations such as declaring a
function and constructing an unresolved call, match, closure, generic application, or effect
syntax—not a public HIR-node ABI and not permission to claim a resolved symbol or verified type.
The common elaborator alone resolves identities, selects evidence, derives ownership/effects, and
constructs compiler-private HIR. Compiler-owned builders may change internal AST/HIR representation
without making independent frontends clients of compiler internals. A frontend may reuse CoLisp's ordinary
syntax, macro, concept, and CTFE libraries by constructing their structured inputs directly, but it
must not emit CoLisp text and invoke another reader. This makes Finch a practical compiler substrate
without turning CoLisp into a second semantic waist or losing the foreign language's source origins
and diagnostics.

Every frontend submits the same types, ownership transitions, effects, and capability requirements
through common elaboration/checking before independent IR verification. A frontend is an untrusted
producer of builder calls and claimed source origins: the construction protocol exposes no way to
mint internal HIR nodes, `FunctionCertified`, `ModuleSealed`, or `ModuleVerified`, and the common
checkers derive or validate every security-relevant fact. Typed stack IR remains the stable
executable/verification representation; compatibility of the frontend protocol is versioned
separately from internal HIR layout.

The retained tree is elaboratable rather than permanently syntax-only. A node keeps its stable
source identity and expansion ancestry while acquiring only the semantic facts needed to lower it:
a resolved symbol, instantiated signature and concept evidence, inferred type/effect summary,
ownership transformation, control-flow successors, or merge-stack contract. For example, an
`UnresolvedWord("foo")` becomes a resolved call/local/control node, an `if` gains typed branches and
its merge row, and a generic call gains definition/evidence identities. Implementations may use
phase-indexed nodes, side tables, or immutable replacement nodes. They should avoid copying a
complete tree for every phase unless a measured performance, concurrency, or semantic need justifies
it. Final typed IR is emitted only after the relevant semantic facts are resolved.

Keep that pipeline deliberately short:

```text
source bytes -- one authoritative parse boundary --> frontend AST
frontend AST --> declarations + typed module interfaces
frontend AST -- elaboration/expansion --> parametric HIR (only where required)
elaborated AST/HIR -- instantiation + post-order lowering --> typed stack IR
typed stack IR -- independent security verification --> executable module
```

### Abstraction boundaries (do not collapse)

There are **two** handoffs. Treating them as one is how frontends end up emitting IR and the VM ends up selecting CoLisp vs Co-Forth.

**1. Frontend → compiler: shared structured syntax, not IR.**

Each frontend owns a private parse tree (CoLisp `Val`/`SpannedVal`, Co-Forth's span-preserving module tree). That tree does **not** enter `finch-vm`, `src/runtime`, Brain, or the TUI. The frontend submits span-bearing structured syntax through the versioned semantic-construction protocol (builder calls: declare a function, unresolved call, match, closure, generic application, effect syntax). That protocol is the **shared** input the dependency scheduler elaborates. It is not a public HIR-node ABI, and it cannot mint `FunctionCertified`, `ModuleSealed`, or `ModuleVerified`.

This is the SDC lesson: parse AST is frontend-private; the compiler scheduler runs on **symbols and phases** derived from those builder submissions (`Declared → SignatureReady → BodyTyped → Lowered → FunctionCertified`). Forward references are `require(symbol, SignatureReady)` (a promise the scheduler fulfills). Parallel files publish skeletons independently. Two files importing one module intern **one** in-flight module job; they do not each lower IR and contend in the VM.

**2. Compiler → execute: typed stack IR, not AST.**

`finch-language` (the compiler door) is the only place that lowers. `finch-vm` (interpreter, fibers, checkpoints), later Cranelift, `programs`, and `src/runtime` consume **`ModuleVerified` IR**. They do not take frontend trees, builder traces, or compiler-private HIR. Application composition submits either source plus a language tag to `finch-language`, or an already-verified module to `finch-vm`. It never compiles by importing `finch-colisp` or `finch-coforth`.

The VM fiber scheduler is a **different** machine from the compiler job scheduler. Compiler jobs may eventually be self-hosted as CoLisp fibers that yield `CompilerNeed`; they still lower to IR before anything executes.

**3. Interpreter and Cranelift: the same IR waist.**

The interpreter and the later Cranelift handoff consume the **same** Finch typed stack IR. They do not take AST, builder traces, or `require(symbol, stage)`. CLIF is a hidden, rebuildable backend IR behind Cranelift; it is not the program-exchange format and is not a second language.

```text
finch-language
  await require(...) → lower → FunctionCertified → ModuleVerified
                         │
                         ├─→ interpreter (tier 0)
                         └─→ CLIF → native (later)
```

Shared across both backends: IR types, verifier, source maps, trap/safepoint metadata, capability request sites, and the effect/resume ABI (native shims stay capability-bound). Not shared: the compiler scheduler, frontend trees, or CLIF.

The interpreter **runs** only `ModuleVerified`. Native lowering **may start** from `FunctionCertified` as a quarantined cache, discarded if the module certificate never issues. An first implementation may wait for `ModuleVerified` for both.

**Crate split that keeps the scheduler shareable.** `require` only works if the elaboratable tree, the symbol/phase table, and lowering live together (`finch-language`, with types in `finch-vm-core`). Frontends do not own that scheduler; they publish builder submissions. `finch-vm` does not own it either: it is an IR backend, like Cranelift. Splitting at IR is the intended waist. Splitting with AST inside `finch-vm`, or with frontends emitting IR, makes `require` impossible or forces every execution change to load the compiler.

**Why not “shared AST into the VM instead of IR”.** Putting the elaboratable tree in `finch-vm` makes the interpreter a compiler, forces every Brain/runtime change to load elaborator state, and gives native lowering a second source of truth. The shared tree belongs to `finch-language`. The shared **executable** waist is typed stack IR. Today’s tree is inverted: frontends still lower privately and the VM still re-exports `compile_forth` / `compile_lisp`. That inversion is debt, not the target.

Shared lowering helpers enforce Lisp/Co-Forth parity without requiring an intermediate tree for its
own sake. Source-defined generics and compile-time templates are the concrete feature that can
justify one small shared parametric HIR: a concrete typed runtime instruction stream is too late to
retain generic parameters, constraints, an unresolved reusable body, module-interface references,
and expansion provenance. The HIR may instead be an explicitly elaborated AST if no separate node
family is useful. It must not become an excuse for a succession of mandatory compiler passes.
Optimization may traverse retained IR and never changes the one-parse source contract.

Modules are compilation units, never textual includes. A module has an immutable identity, typed
imports and exports, a namespace, a compiled interface, IR, source map, and content hash. Importing
a module links its declared interface/IR; it does not paste source, execute ambient initialization,
or confer capabilities. Self-contained model-authored scripts remain the default when an import
would make an artifact harder to audit.

An import is an ordinary lexical declaration wherever declarations are allowed, including inside a
function, quotation body, compile-time function, or nested block. It is not restricted to a special
module header and is never a runtime call. A local import affects only the containing lexical scope
and its descendants, beginning after the declaration appears; it cannot be forward-referenced from
an earlier expression. A declaration inside a conditional branch is still an unconditional
compile-time dependency whose names are visible only in that branch's lexical block—it does not
perform conditional runtime loading. Module-level imports participate in the module declaration
frontier and may be resolved without source-order significance.

Imports have three explicit binding forms:

```text
whole module       import codec.json
namespace alias    import codec.json as json
selected names     from codec.json import encode, decode as decode-json
```

A whole-module import makes its public names available as imported candidates and retains a
qualified module binding. A namespace alias exposes only the qualified alias. A selective import
binds only the listed exported symbol identities or overload groups, optionally renamed. Selection
can name functions, types, concepts, evidence, syntax transforms, or compile-time values; their
phase and type remain those published by the immutable interface. There is no textual wildcard
expansion, runtime reflection search, or import inferred merely because an unresolved spelling
happens to exist in a dependency.

Name resolution searches direct lexical declarations before imported candidates, and searches
imports from the innermost lexical scope outward. Two imported candidates in the same selected
scope are an ambiguity regardless of import order; qualification, selection, or renaming resolves
it. An inner scoped import may hide an outer imported candidate but never silently replaces a direct
local binding. Operator/concept coherence remains stricter: loading a module does not make all of
its evidence ambient, and operator-default evidence still requires the explicit/default selection
rules described below.

Every reached import records the exact immutable module/interface identity and phase dependency in
the semantic job graph and sealed module. Local scope reduces name pollution and compiler working
context; it does not hide a dependency from hashing, cycle detection, reproducibility, capability
review, or diagnostics. An imported binding captured by a closure is a stable symbol/module
reference, not a runtime captured module object. Local imports cannot be re-exported; public
re-export is an explicit module-scope interface declaration.

Import resolution never behaves like textual `#include`. The compiler service interns one module
identity and deduplicates concurrent requests for it. Source bytes are read into a parsed module at
most once per `(content hash, source language, reader version)` cache key; elaborated interfaces,
verified parametric artifacts, and `ModuleVerified` results use successively stronger keys that also
include compiler semantics, dependency interfaces/evidence, target-independent policy, and relevant
feature versions. Ten whole, aliased, or selective imports therefore create ten scoped binding views
over one immutable module artifact, not ten copies, parses, semantic jobs, allocations, or module
instances.

An in-flight module job is shared: later importers await its declared phase rather than starting a
duplicate parse or compiler pipeline. A completed artifact may be memory-resident, memory-mapped, or
reloaded from a content-addressed cache without changing identity. Eviction affects performance only.
Changed source, compiler semantics, dependency interface, or evidence invalidates the appropriate
downstream artifact; it never mutates an old imported module in place. A remote or persisted
`ModuleVerified` artifact is independently validated before reuse. Scoped import is therefore a
name-reachability operation over cached immutable compilation data, not ownership of an allocated
runtime module.

Canonical paired spellings are:

```lisp
(define (encode-user user)
  (begin
    (import codec.json :as json)
    (json/encode user)))

(define (read-user text)
  (begin
    (from codec.json :import (decode (JsonError :as DecodeError)))
    (decode text)))
```

```forth
: encode-user ( S borrow User -- S string )
  import: codec.json as json ;
  json.encode
;

: read-user ( S borrow string -- S User ) throws DecodeError
  from: codec.json import{ decode JsonError as DecodeError } ;
  decode
;
```

Both frontends construct the same scoped `ImportDeclaration` semantic node. The exact surface may
be refined with the grammar, but must preserve whole-module, alias, selective, renamed, lexical
scope, source-order, and phase behavior without source-to-source rewriting.

An imported module is immutable code and interface identity, not an implicitly created process
singleton. Module constants whose initializers are pure, bounded, and compile-time-known are folded
into the sealed artifact. Mutable or effectful runtime state is constructed explicitly and has an
ordinary owner. A module can export `new-state`, a service constructor, or functions accepting a
borrowed/shared state record; importing those functions at ten lexical sites neither constructs ten
states nor secretly selects one global state.

At-most-once initialization is a standard-library ownership/synchronization policy rather than an
import side effect. Conceptually:

```lisp
(record CodecState
  (:service (Once CodecService)))

(define (new-codec-state)
  (CodecState :service (once-cell)))

(define (codec (state : (borrow CodecState)))
  (once-get-or-init (. state service)
    (lambda () (open-codec-service))))
```

```forth
record: CodecState fields{ service: Once<CodecService> } ;

: new-codec-state ( S -- S CodecState )
  Once<CodecService>{} CodecState{ service: }
;

: codec ( S borrow CodecState -- S borrow CodecService )
  .service [ open-codec-service ] once-get-or-init
;
```

The application, actor, test, or explicit module-instance record owns `codec-state` and shares that
owner with the functions that require the same initialization domain. A host may offer an explicit
runtime service registry keyed by immutable module/service identity when process- or Brain-scoped
singleton behavior is genuinely required, but acquiring it is a typed host effect and dependency,
not a consequence of name lookup. This makes “once per process,” “once per Brain,” “once per task,”
and “once per explicit instance” distinguishable rather than accidental.

`Once<T>` has a specified concurrent state machine: empty, initializing, ready, or terminal under a
chosen failure policy. Waiters park through the resumable scheduler rather than spin; recursive
initialization reports a dependency cycle. Retry-after-failure, memoized failure, cancellation
handoff, and suspending versus non-suspending initializers are explicit policies. The initializer's
effects, exceptions, capabilities, and suspension remain visible in the calling contract. The
stored value drops when its owning cell/service instance drops, not at an unspecified module-unload
phase. `Once<T>` can therefore be implemented as a library owner/synchronization type over the
language's atomics, drop, and suspension primitives without compiler-owned module constructors.

Semantic analysis should be dependency-driven rather than implemented as repeated whole-module
passes. As soon as the parser publishes a stable declaration/body node, its symbol may own a bounded
semantic job even while later source is still being parsed. A job blocked on a later declaration
waits for that skeleton. Reaching EOF closes the parse frontier, but not necessarily the declaration
frontier: visibility-eligible macro expansions and imported interfaces may still publish structured
declarations through the same registry. Absence is diagnosed only after that bounded expansion and
declaration frontier closes. Dependencies among expansions use the same phase-aware cycle detection
rather than schedule-dependent early failure. Each symbol and generic instantiation advances
through monotonic readiness phases:

```text
Declared -> SignatureReady -> BodyTyped -> Lowered -> FunctionCertified
```

The scheduler API is `require(symbol, stage)`, not an event bus. It returns a promise whose
other end the scheduler owns; analysis **awaits** that promise. There are no
`SymbolReady` pub/sub listeners, callback tables, or ad hoc “pass completed” broadcasts.

```text
let sig = await require(f, SignatureReady);
// type-check this call against sig
let ir  = await require(f, Lowered);
```

What that means operationally:

- If `f` is already at `stage`, the promise is already resolved.
- If not, the current job parks and the scheduler runs `f`'s job until that stage (or a cycle/fuel
  failure). Interning means a second `require(f, stage)` awaits the **same** promise, which is how
  two files importing one module share work instead of contending.
- Mutual recursion is legal when both sides can reach `SignatureReady` without each other's
  bodies. Awaiting a job that is already awaiting you at an unmet stage is a cycle; report the
  full chain with source origins.
- Lowering is just `await require(self, BodyTyped)` then emit stack IR, then
  `FunctionCertified`. It is not a second scheduler.

This is the same graph SDC encodes with stackful fibers so `require` looks synchronous
([`scheduler.d`](https://github.com/snazzy-d/sdc/blob/master/src/d/semantic/scheduler.d)). Finch's
Rust bootstrap should use explicit promises/jobs (`Ready` vs `Needs { symbol, stage }`) rather
than native fiber stacks: deterministic order, cycle traces, fuel, cancellation, and tests
without a second stack runtime. The self-hosted compiler may later write the same `await require`
as a CoLisp fiber that yields `CompilerNeed`; that is still this scheduler, not the VM's runtime
fiber scheduler and not an event architecture.

Generic instantiation creates or reuses a synthetic job keyed by immutable module and
definition identity, type/value arguments, and resolved concept evidence. That job may emit shared
evidence-passing IR or a selected concrete specialization; semantic resolution does not require
code multiplication.

Compilation may therefore be pipelined rather than separated by whole-module barriers:

```text
source -> parsed AST regions -> dependency-ready semantic jobs -> typed IR functions -> verifier
```

Lowering consumes resolved syntax in evaluation order and produces compact typed stack
transformations; local verification of a completed function does not require retaining unrelated
source bodies. It checks the function's instructions, control-flow, stack and ownership invariants,
and calls against exact immutable dependency summaries. Parsing may continue while earlier jobs
elaborate, and completed definitions may lower and enter `FunctionCertified` while others wait
for signatures or evidence. Synchronization occurs at real semantic boundaries: macro visibility,
signatures, evidence and layouts, closed control-flow graphs and merge rows, inferred recursive
summaries, imports, and the final module interface. Publication remains atomic and requires the
complete module composition/security verifier result, including transitive effects, capability
requirements, dependency versions, interface agreement, and a certificate keyed to the final module
hash. A partially compiled module is never externally visible or executable.

Use security-explicit names for these compilation states. `FunctionCertified` means the local structural,
type, stack, ownership, and exact-dependency checks above passed; it permits quarantined downstream
compilation but never execution. `ModuleSealed` means the declaration/expansion graph is closed,
exports and summaries are frozen, and content identity can be assigned. `ModuleVerified` means the
independent composition/security verifier accepted that sealed module and issued its final
certificate. Only `ModuleVerified` permits publication, import, or execution; the states are distinct
types rather than interchangeable flags.

Native lowering may begin from `FunctionCertified` IR only as a provisional cache artifact.
Its key includes the exact function IR, dependency summaries, compiler/runtime ABI, target, and
optimization policy; any changed input invalidates it. The artifact remains quarantined until the
final module certificate validates its composition, and Cranelift still never consumes unchecked
IR. An initial implementation may simply defer all JIT work until module verification if provisional
compilation does not produce a measured win.

Parallel scheduling is an optimization, not a semantic input. Stable identities derive from module
and source structure rather than job completion order; diagnostics have deterministic ordering; and
single-threaded, shuffled, and parallel schedules must produce byte-equivalent interfaces and IR.
Jobs should be coarse enough to amortize scheduling cost, queues must apply bounded backpressure,
and a small module may run serially. This preserves the speed goal without turning individual AST
nodes into synchronization-heavy actors.

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
9. Ordinary application code feels like statically checked scripting: local types, generic
   evidence, safe borrows, representation-independent comparisons, and profitable specialization
   are inferred without hiding allocation, erasure, encoding changes, authority, or ownership
   escape.

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

### Scripting ergonomics with a systems cost model

Finch aims to provide nearly systems-level layout, ownership, compilation, and native-code control
while making ordinary code read like a scripting language. Most private code should not mention a
type that is already determined by its initializer and immediate arguments. The compiler performs
directional local inference, inserts proven scoped borrows and readonly views, resolves declared
concept evidence, specializes when measured to help, and removes bounds/ownership checks it proves
redundant. Errors remain attached to the expression that introduced the incompatible value rather
than a distant constraint solution.

“Automatic” does not mean cost-blind. An implicit adaptation may prove a fact, copy a `Copy` scalar,
or create a scoped zero-copy view. It may not allocate, retain shared ownership beyond the call,
change text encoding, erase static evidence, perform runtime name lookup, acquire authority, or move
an owner unless the source construct or receiving contract makes that cost visible. Literals and
operations whose purpose is construction may allocate according to their declared result type; the
optimizer may keep them inline or remove the allocation when unobservable.

Compiler-recognized syntax is kept smaller than the standard library. The compiler owns literal
syntax, static array/tuple layout, borrow and slice validity, checked indexing, and UTF-8 literal
validation because they affect parsing, safety, or ABI. Public concepts expose sequence iteration,
comparison, literal construction, text views, ranges, and collection building so user-defined types
can receive the same generic algorithms and most surface convenience. No standard-library type gets
a private method lookup or comparison path unavailable through those contracts.

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

The stack form is also a compiler-pipeline boundary, not merely an accommodation for Co-Forth. Each
instruction compactly states its input/output rows, ownership transition, effect contribution,
control successors, and source origin. Once a function's resolved syntax has been linearized into
those transformations, local verification, serialization, interpretation, and SSA lowering no
longer need its unrelated frontend tree. This makes functions natural bounded producer/consumer
units for dependency-driven and parallel compilation while preserving deterministic evaluation
order.

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
string           immutable owned valid-UTF-8 text value
bytes            immutable owned contiguous byte sequence
array<T,N>       fixed-length inline homogeneous product
slice<T>         scoped readonly contiguous view
slice-mut<T>     scoped exclusive contiguous view
vector<T>        uniquely owned growable contiguous sequence
path<R>          normalized path proven relative to root R
list<T>          immutable persistent sequence
map<K,V>         managed mapping
tuple<T...>      anonymous fixed-length heterogeneous product
option<T>        standard-library closed variant: none or some(T)
result<T,E>      standard-library closed variant: ok(T) or err(E)
record{...}      named product type
variant{...}     tagged sum type
word<S,E>        callable word with stack signature S and effects E
fn(P...)->R       lexical callable; P includes ownership modes and the full callable contract
task<T>          scheduler-owned child/task handle
stream<T>        scheduler-owned lazy sequence/cursor handle
fiber<Y,Resume,R> resumable producer that yields Y, accepts Resume, and returns R once
resource<K>      generation-bound runtime handle
capability<C>    unforgeable grant handle; never synthesized from text
dynamic          explicitly tagged escape hatch
unique<T>        library-defined sole owner of heap storage
shared<T>        library-defined strong reference-counted owner
weak<T>          non-owning shared-storage handle; checked upgrade
```

Integers, booleans, floats, characters, and small handles should remain unboxed where the target
ABI permits it. Strings, collections, closure environments, and larger records use an explicit
ownership carrier when they outlive an inline frame value. Storage policy is independent of value
type: safe heap storage is always uniquely or sharedly owned, while a plain lexical value remains
frame-owned. `dynamic` carries a tag and requires checked narrowing. Static code must not pay
dynamic dispatch cost merely because the Lisp frontend exists.

The serialized `ProgramValue` form is the wire/checkpoint representation, not necessarily the
in-memory stack layout.

### Text, arrays, slices, vectors, and lists

Text and collections share public sequence concepts without pretending to share one representation:

| Type | Length/layout | Ownership and mutation | Primary use |
|---|---|---|---|
| `array<T,N>` | `N` in the type; values inline | owned value; fixed length | stack fields, ABI-shaped buffers, small fixed data |
| `slice<T>` | runtime length; contiguous view | scoped readonly borrow | zero-copy input over arrays/vectors/bytes |
| `slice-mut<T>` | runtime length; contiguous view | scoped exclusive borrow | checked in-place algorithms |
| `vector<T>` | runtime length/capacity; contiguous | unique owner; exclusive mutation/growth | dynamic arrays and buffers |
| `list<T>` | runtime length; non-contiguous allowed | immutable persistent owner | structural sharing and list algorithms |
| `bytes` | runtime length; contiguous octets | immutable owner | binary and wire data |
| `string` | runtime byte length; valid UTF-8 | immutable owner | human text |

These are logical value types, not framework storage classes. Backing storage may use the ordinary
owner carriers and may be kept inline or eliminated when unobservable. `slice` and `slice-mut` never
own or extend storage; their origin is checked by the ordinary borrow rules, and they cannot escape,
survive suspension, or become retained native addresses without owner and pinning evidence. A
mutable vector operation that may grow invalidates outstanding views, which is already prevented by
the exclusive-borrow rule. Persistent-list updates return a new list. Shared ownership never grants
mutation.

`Sequence<T>` provides finite readonly traversal. Independent concepts provide runtime sizing,
`KnownLength<N>`, contiguity, random access, mutable access, growth, and ownership. Algorithms state
only the evidence they require: equality needs two finite readable sequences and compatible element
equality; a native write needs contiguity plus a stable borrow; append-in-place needs unique growable
storage. Built-in and user-defined collections publish explicit mappings through the same concept
system. Compiler syntax may select an optimized loop from evidence but cannot use a private
standard-library-only operation.

Sequence equality is an allocation-free algorithm, not a coercion. Arrays of different static
lengths are unequal without inspecting elements. If either length is dynamic, equality compares
lengths and then corresponding elements. Arrays, slices, vectors, and lists may therefore compare
directly whenever their element types have compatible `Equal` evidence; no representation conversion
or temporary copy occurs. Lexicographic ordering and hashing use similarly explicit evidence, and
map keys require sealed `HashKey` evidence combining an equivalence relation with a consistent hash.
Arbitrary lazy or potentially infinite ranges do not receive equality implicitly because comparison
could consume effects or fail to terminate.

Indexing syntax performs a checked lookup and produces a defined bounds trap containing the index,
length, and source origin. `.get` instead returns `option<&T>` when absence is ordinary data. Slicing
checks its endpoints once and returns a scoped view. Static lengths, loop refinements, and preceding
guards allow the verifier/JIT to remove redundant checks; safe source does not need unchecked
indexing for performance.

A collection literal becomes one span-bearing `SequenceLiteral<T,N>` syntax node. Homogeneous
elements infer `T` locally; empty or intentionally mixed literals require an expected element type
or an explicit variant/dynamic type. An expression-local expected type may select `array<T,N>`,
`vector<T>`, `list<T>`, `bytes`, or a user-defined `FromElements<T,N>` implementation. Without such
an expectation, the scripting-oriented default is `vector<T>`; fixed arrays, persistent lists, and
bytes use explicit constructors. Elements evaluate exactly once from left to right. Literal
construction may allocate because construction is visible, but converting an existing owner to a
different representation is explicit (`to-vector`, `to-list`, or equivalent). A compatible
array/vector/bytes value may implicitly lend a zero-copy slice because that adaptation only borrows.

Canonical paired spellings include:

```text
CoLisp:   (array 1 2 3)    (vector 1 2 3)    (list 1 2 3)    (bytes #x00 #xff)
Co-Forth: array{ 1 2 3 }   vector{ 1 2 3 }   list{ 1 2 3 }   bytes{ 0x00 0xff }
```

Spread into a fixed array requires compile-time-known cardinality; runtime spread is accepted only
by a growable builder. A uniquely owned `StringBuilder` or `VectorBuilder<T>` exposes mutation and
then `freeze` consumes it into immutable `string`, `bytes`, or another selected collection. Freeze
may reuse its allocation, invalidates the builder, and creates no second owner. Copy-on-write and
shared builders remain explicit policies rather than literal magic.

Ordinary string concatenation constructs one owned string and is linear in the combined byte
length. A chain of concatenations may be fused into one builder allocation only when evaluation
order, exceptions, and observable allocations remain equivalent. Incremental or loop-based
construction uses `StringBuilder` explicitly, so convenient `str-cat` does not imply a quadratic
implementation or a hidden mutable string type.

`string` is the sole core Unicode text value: immutable, movable, and guaranteed to contain valid
UTF-8. A string literal is validated by the reader and may use immutable module backing without a
runtime allocation. `Text` is a public readonly-view concept rather than another owning string
class; `string`, literals, and foreign/framework carriers can map a scoped allocation-free text
borrow into it. Readonly APIs accept `borrow Text`; an API retaining text accepts or constructs an
owned `string`. A substring is a scoped text borrow by default, and escaping it requires explicit
materialization or an owner-carrying view, avoiding both hidden reference counts and accidentally
retaining a huge source buffer for a tiny substring.

Exact string equality and hashing compare Unicode scalar content, equivalently the bytes of two
already-valid UTF-8 values. They do not normalize, case-fold, or apply locale collation implicitly.
Those operations are explicit versioned library policies because they may allocate and Unicode-data
versions affect results. Nul is an ordinary scalar in a Finch string. Strings and bytes do not
compare implicitly merely because a string has a
UTF-8 representation; viewing a string as readonly UTF-8 bytes is zero-copy, while decoding arbitrary
bytes validates and returns `result<string,Utf8Error>`. Lossy decoding is explicitly named.

Integer `string[i]` is intentionally absent: in Unicode, “character” may mean a byte, scalar, or
grapheme cluster, and scalar/grapheme indexing is not constant-time in UTF-8. `.bytes`, `.chars`, and
`.graphemes` expose explicit ranges; `byte-count`, `scalar-count`, and `grapheme-count` name their
units. Byte slicing must preserve scalar boundaries or return an error. Grapheme operations state a
Unicode-data version. Source escapes include `\\`, `\"`, `\n`, and `\u{...}` and reject surrogate
values. Both frontends accept `r"..."`, with matching hash-count fences such as `r#"..."#` and
`r##"..."##`, so embedded quotes and shorter fence sequences need no escaping. The reader records
the chosen fence but the resulting value is the same `string`; raw spelling does not create another
string type.

Constant-pattern matching is generic semantics with representation-specific lowering. A constant
arm uses certified pure, total, deterministic, non-suspending `PatternEqual` evidence; optional
consistent `PatternHash` evidence permits hashed dispatch. Closed variants may use tag jump tables,
dense integers may use value jump tables, and strings may use length buckets, tries, or perfect/hash
tables followed by equality for collision checks. Other library-defined key types can receive the
same optimization by publishing the evidence. Matching strings therefore requires no hidden type
byte and no string-only source rule. An open domain such as string or integer still requires a
catch-all arm for exhaustiveness.

Framework and FFI boundaries use views and ownership protocols rather than inventing string
classes. The portable Finch ABI represents borrowed text/bytes as pointer-plus-length views with an
explicit call-scoped lifetime, and owned results as versioned owner handles with a matching release
operation. C strings, UTF-16 platform strings, nul termination, normalization, and foreign allocator
ownership require named adapters. An adapter borrows without allocation only when encoding,
termination, lifetime, and address stability already satisfy the target; otherwise its scoped
allocation or owned result is visible in the contract. Native `string`, vector, and list layouts
never become the C or stable Finch ABI. `repr(C)` fixed arrays require C-safe elements; stable records
embed managed collections only through specified stable handles or encodings.

Wire and checkpoint encodings are likewise logical rather than native layouts: strings encode a
length plus validated UTF-8 bytes, `bytes` encode length plus octets, arrays include and validate
their declared static length, and vectors/lists encode an ordered element sequence under a versioned
element schema. Decoding enforces size/allocation budgets before construction and never restores a
native pointer, capacity, reference count, or framework string object from serialized data.

### Operators, comparison evidence, and segmented text

Operator punctuation is syntax for named public concept operations, never privileged member lookup.
An operator expression records the token, both operand types, selected evidence identity, and source
span before lowering to an ordinary concept call. The initial token and precedence table is fixed by
the language version; libraries may define evidence for new type pairs but cannot invent punctuation
or precedence. This keeps parsing deterministic and makes operator behavior available to macros,
foreign frontends, the interpreter, and native lowering through one semantic path.

Binary concepts name both operand positions and may produce an associated result:

```text
concept Equal<L,R> symmetric {
    operation equal(borrow left: L, borrow right: R) -> bool
        guarantees pure total deterministic non-suspending
}

concept Compare<L,R> {
    associated Ordering
    operation compare(borrow left: L, borrow right: R) -> Ordering
        guarantees pure total deterministic non-suspending
}

concept Add<L,R> {
    associated Output
    operation add(borrow left: L, borrow right: R) -> Output
}
```

`symmetric` is the correct law for a relation: `equal(a,b)` implies `equal(b,a)`. It is an explicit
evidence-generation rule, not a guess based on an operation's name. One `Equal<A,B>` implementation
supplies a compiler-generated `Equal<B,A>` adapter that swaps the arguments while retaining the same
sealed evidence identity. Defining both directions independently is therefore an overlap error
unless one is explicitly named non-default evidence. `commutative` is the distinct law for a binary
operation: `op(a,b) == op(b,a)`. Ordered concepts such as `Add<L,R>` and `Compare<L,R>` do not imply
their reverse; a numeric library may declare commutativity only where that law is actually valid.
`!=` is derived by negating selected equality evidence, and `<`, `<=`, `>`, and `>=` derive from one
selected comparison operation rather than admitting six unrelated implementations.

Algebraic structure is expressed by evidence that bundles operations and their laws, not by making
every operator symmetric or attaching arithmetic inheritance to a record. A `Ring<T>` can require
additive commutativity while leaving multiplication ordered; a `StarAlgebra<T,Scalar>` can add scalar
multiplication and an involution whose law reverses product order; a `CStarAlgebra<T,Scalar>` can add
norm and completeness contracts. The underlying `T` may be an ordinary record. Its implementation
maps each operator to named callable evidence and can be selected statically or carried in a `dyn`
view like any other concept implementation. Generic algorithms can request the complete algebraic
bundle when their reasoning needs those coherent laws, or only `Add<T,T>` when it does not.

Law vocabulary distinguishes the shapes involved rather than attaching a vague `algebraic` marker:

- relation laws include reflexivity, symmetry, transitivity, and antisymmetry;
- one-operation laws include commutativity, associativity, identity, absorption, and idempotence;
- an involution names a unary operation and states that applying it twice returns the original;
- distributivity names both operations and the direction of distribution;
- anticommutativity names the required negation evidence (`op(a,b) == negate(op(b,a))`) rather than
  treating it as reversed dispatch;
- anti-homomorphism laws can state, for example, the star-algebra rule
  `adjoint(mul(a,b)) == mul(adjoint(b),adjoint(a))`.

These laws belong to a particular evidence bundle, not globally to the operator token or record
type. The same record can participate in several explicitly selected algebras with different
operations or policies.

Mathematical linearity is named `LinearMap<V,W,Scalar>` (or an equivalently explicit concept), not
`linear`, because Finch also uses *linear* to describe ownership that must be consumed exactly once.
Its law relates the selected vector-addition, scalar-multiplication, and application evidence. A
`HermitianOperator<V,Scalar>` is a linear endomorphism with selected inner-product and adjoint
evidence and the self-adjoint law. A `UnitaryOperator<V,Scalar>` additionally states the appropriate
adjoint/inverse or inner-product-preservation law. These are structured multi-operation contracts,
not callable flags.

Some properties attach to a particular value rather than every value of its representation. A
general `Matrix<T,M,N>` type is not Hermitian or unitary merely because some instances are. A
checked constructor such as `try-as-unitary` may validate runtime coefficients and return a refined
owner/view carrying sealed `UnitaryOperator` evidence; trusted construction from statically proven
combinators may produce the same refinement without a runtime scan. Mutation invalidates
value-specific evidence unless the mutating operation proves that it preserves the property.

The compiler enforces the mechanically decidable portion of a law bundle: operation types,
ownership/effects, associated-type agreement, evidence coherence, and declared derivations such as
argument reversal. It does not claim to prove arbitrary algebraic identities or analytic properties
from function bodies. A law declaration is an auditable API contract that generic code may require,
but an unchecked declaration is never an optimizer certificate. Optimizer-visible certificates come
only from compiler-derived structural proofs, proof artifacts checked by a small trusted verifier,
runtime-validated refined evidence, or explicitly unsafe trusted-library admission outside hosted
profiles. Property tests remain valuable diagnostics but are not proofs. Consequently, falsely
marking an arbitrary function `linear` cannot make safe hosted code silently combine inputs and
miscompile the result.

Optimizations may rely on certified laws admitted by the active safety/profile policy only when the
rewrite also preserves operand evaluation, exceptions, ownership, and observable destruction.
Linearity can enable certified map fusion and distribution; Hermiticity and unitarity can select
specialized kernels, eliminate certified adjoint/inverse pairs, or preserve known norm facts.
Associativity therefore does not apply to ordinary IEEE floating-point addition, and reassociation
of checked arithmetic is invalid when it could change which operation traps. A separate approximate
or fast-math policy may deliberately expose different evidence rather than weakening strict source
semantics globally.

Operator selection uses only operand types plus lexically explicit or uniquely canonical evidence.
It never uses the expected result type, import order, receiver/member position, or speculative body
compilation. At most one implementation for a concept/type tuple may be the operator default in a
scope. Alternative policies remain available through an explicit `using` selection, a concept-
qualified call, or a policy wrapper type. Thus two serialization or numeric policies can coexist
without punctuation silently changing meaning.

Operator equality is allowed to be non-reflexive for domains such as IEEE floating point. A map key
requires the stronger `Equivalence<T>` law (reflexive, symmetric, and transitive) together with a
compatible `Hash<T>` implementation; `HashKey<T>` bundles and seals that evidence. This prevents a
convenient `==` implementation from accidentally defining an invalid hash-table key relation.

`Text` exposes a readonly sequence of valid-UTF-8 byte chunks as well as its logical scalar content.
A rope, flat `string`, borrowed substring, or foreign text view can therefore implement the same
public comparison evidence without flattening. Exact text equality first rejects known unequal byte
lengths, then walks overlapping chunk regions and uses bytewise comparison within each region. It is
segmentation-independent, allocation-free, linear in compared bytes, and uses constant auxiliary
space. Equal valid-UTF-8 byte sequences are equal scalar sequences, so this optimization preserves
the language rule; normalization, case folding, locale collation, and `bytes`/text conversion remain
explicit policies. Hashing streams the same logical bytes across chunk boundaries so equal flat and
segmented text values hash identically. Immutable views with proven identical owner/range identity
may short-circuit before reading bytes.

For other sequences, a certified `BytewiseEqual<T>` property permits bulk comparison of contiguous
regions only when every bit pattern and padding rule makes that operation semantically equivalent to
element equality. Otherwise comparison invokes element evidence. Arrays with incompatible static
lengths reject immediately; same-length arrays and slices with already-proven equal lengths omit a
runtime length branch; dynamically sized inputs check length once. Rope chunks can still use bytewise
comparison because their element is an octet even though the whole value is non-contiguous.

Canonical spellings are ordinary operator forms in both frontends:

```text
CoLisp:   (== rope text)       (+ duration offset)
Co-Forth: rope text ==         duration offset +
```

Both forms construct the same operator semantic node and resolve the same evidence. The named forms
remain available for explicit policy selection; punctuation is convenience, not an otherwise
inexpressible operation.

### Records, layout, placement, and member access

`record` is the one user-defined product aggregate in core CoLisp and Co-Forth. There are no
separate class, struct, heap-object, or reference-record declarations. A record definition owns its
logical fields, visibility, construction invariants, and layout contract; it does not select stack
versus heap placement, an ownership policy, or static versus dynamic behavioral dispatch. The same
`Foo` may therefore be frame-owned inline, embedded inside another record, placed behind
`Unique<Foo>` or `Shared<Foo>`, or borrowed through any of those carriers without becoming a
different aggregate type.

Named records have nominal identity. Matching field names do not make independently declared
records interchangeable, because their invariants, constructors, lifecycle evidence, and layout
contracts may differ. Structural width conversion is available only through an explicit readonly
record-view type/evidence that borrows selected fields; it does not reinterpret or copy a record by
layout and never applies implicitly across `repr(C)`, `repr(stable N)`, mutable, or owning storage.
`tuple<T...>` is the anonymous structural product for fixed heterogeneous values such as selected
parameter packs and `join-all` results; it has positional fields, derived lifecycle evidence, no
nominal identity, and no declaration-owned invariants.

Native record layout is compiler-owned and may evolve with the compiler/runtime ABI. An explicit
layout declaration chooses a stronger contract when required: conceptually `repr(native)` is the
optimized default, `repr(C)` follows the target C ABI, and `repr(stable N)` freezes a versioned Finch
module/FFI representation. Explicit packing, alignment, field-offset, or validity claims are unsafe
layout contracts. Exported opaque records expose operations without exposing offsets. Published
non-opaque layouts contribute their representation version, size, alignment, field order, and
calling convention to the module interface and compatibility hash.

Human-facing member syntax uses one `.` operation; there is no separate `->`. Member access and an
ordinary borrowed call may apply the same bounded safe receiver projection: a direct `Foo`, a
scoped `&Foo`, or a uniquely selected `Owner<Foo>` borrow projection all present the same logical
receiver. Resolution tries a direct member first, then one proven borrow projection, and reports
ambiguity rather than following an unbounded user-defined dereference chain. Mutable access must
produce an exclusive `&mut Foo`; `Shared<Foo>` cannot do so merely because it can produce `&Foo`.
A raw pointer is never an automatic safe projection.

Records and maps remain different representations. A record field has a compile-time type and
offset and cannot be absent. A `map<K,V>` performs runtime key lookup. Repeated dynamic shapes may
later receive JIT shape/offset caches, but static records do not begin as hash maps and pay to
recover their declared structure.

`option<T>` and `result<T,E>` are ordinary standard-library definitions over the general closed
variant facility. The compiler privileges neither their names nor their constructors. They gain
exhaustive destructuring from normal pattern matching and may expose ordinary `map`, `and-then`,
collection, and traversal operations through concepts. Constructing or returning `err(E)` does not
throw, propagate, attach diagnostic context, or alter control flow unless source explicitly converts
that value into an exception.

### Closed variants, representation, and destructuring

Pattern matching narrows and destructures a value; it does not imply heap boxing. A closed variant
logically has a discriminant and storage large/aligned enough for its largest payload. The matcher
tests that discriminant, proves which constructor is active on the selected edge, and binds fields
at their statically known offsets and types. Matching a borrowed variant borrows its selected
payload. Matching an owned variant moves non-copyable bindings or copies explicitly copyable ones
as the pattern requests, disarms moved fields, and preserves exactly-once cleanup for every
unselected or unbound field.
Field-moving patterns are rejected for a type with a user-defined whole-value destructor unless an
explicit consuming decomposition operation transfers both its fields and cleanup obligations;
borrowed matching remains valid. This prevents a destructor from observing moved storage without
silently skipping deterministic cleanup.

Physical layout is an optimization contract separate from logical matching. The compiler may store
an explicit compact tag, fold tags shared by nested variants, or use an invalid payload bit pattern
as a niche. For example, a non-null owning/borrowed pointer has a null niche that can represent the
empty arm of an option without increasing its size. This optimization applies to any eligible
library-defined closed variant, not specially to `option` or `result`. User-defined validity/niche
claims are unsafe layout contracts and must be verified at their declaration boundary.

Ordinary statically typed primitives, records, owners, and static concept arguments carry no
runtime type tag merely because pattern matching exists. Closed variants carry only the evidence
needed to select their constructor. Explicitly erased `dyn` values carry their versioned evidence
table and runtime type identity; thrown values carry type identity in the exceptional-transfer
envelope. FFI, persisted, and wire types choose a declared stable representation instead of relying
on compiler-private niche/layout choices. The independent verifier rejects an invalid discriminant
or payload state before safe code can destructure it.

### Typed stack signatures

Signatures use row polymorphism so a word states what it consumes while preserving unknown values
beneath it:

```text
dup          forall A: Copy, S. (S consume-value A -- S A A) ! CopyEffects<A>
drop         forall A: Drop, S. (S take A -- S) ! DropEffects<A>
+            forall S.   (S consume-value int consume-value int -- S int) guarantees pure
file.read    forall R S. (S borrow path<R> -- S path<R> bytes) ! fs.read<R>
agent.await  forall T S. (S take task<T> -- S result<T,agent-error>) ! agent.await
yield        forall Y Resume S. (S take Y -- S Resume) ! yields<Y,Resume>
```

The stack arrow describes values retained or removed from the logical operand stack. Source
callables expose only the ownership modes programmers act on: `borrow T` receives a scoped `&T`;
`borrow-mut T` receives an exclusive scoped `&mut T`; and `take T` consumes ownership. An ordinary
source parameter defaults to `borrow`. Typed stack signatures additionally use `consume-value T`
as a lowering-level cell mode: the instruction pops an independent value already materialized on
the operand stack. It is deliberately not named merely `value`, because it describes consumption,
not the value's nominal type or storage placement.

Call lowering may satisfy `consume-value T` by copying a `Copy` binding, so arithmetic need not move
the caller's lexical scalar. A non-`Copy` binding cannot be silently materialized this way; an
ownership transfer is written and typed as `take`. Yield likewise takes its payload because the
resumable execution may retain it beyond the current activation. In Co-Forth the operand stack owns
its cells: applying a borrowing callable to an owned top cell retains that owner and creates a
distinct scoped borrow operand, whereas `take` or `consume-value` consumes the indicated cell. The
surface transform therefore also shows the borrowed owner in its output row; lowering creates a
transient borrow cell for the callee and destroys only that cell on return. There is no word-specific
implicit choice based on spelling. `dup` therefore requires explicit `Copy` evidence (and retaining
a `Shared<T>` is its copy operation); it cannot duplicate a `Unique<T>`.

`!` introduces one canonical typed effect row. Capability requirements, suspension, mutation,
nondeterminism, and other observable behavior are distinct tagged members of that row, not
unrelated annotation systems. The broker selects capability-bearing members for authorization; the
verifier, optimizer, scheduler, and generic reflection inspect the whole row. Omitted `!` requests
inference rather than asserting an empty row. `pure` is not itself a row member or an alias for
emptiness: it is a verifier-derived predicate over the resolved row and body. Source may request
that proof with a separate `guarantees pure` clause; `! pure` is invalid. A deterministic
function may throw and remain pure but partial; a function may separately be total, `nothrow`,
deterministic, and non-suspending. Generic constraints can require those predicates explicitly. A
yielding callable remains a scheduling barrier even when it performs no mutation or host operation.
Optimizers treat `yields<Y,Resume>` as a control barrier, while the scheduler uses its typed payload
and resumption contract. Source spells row union with `|`, for example
`! fs.read<R> | yields<Progress,unit>`. Within a signature this pipe belongs to the
effect-row grammar, not general control flow. The reader canonicalizes member order immediately so
spelling order does not affect type identity. Separate readable clauses may be accepted as surface
sugar only if they normalize into that same row value.

Exceptions are not value types or source-written members of this effect row. `throw` is a control-
flow edge carrying an ordinary typed value. The compiler infers the set of values that may escape
each callable and records a compact exception summary in HIR, IR, and compiled module interfaces for
handler checking, diagnostics, and optimization. Private source normally declares no exception list.
A published callable must choose `nothrow`, an explicit `throws A | B` upper bound, or an explicit
`throws infer` contract that accepts its exact inferred set. This clause is control-flow/interface
metadata, not a value type, generic `throws<E>`, or effect-row member. Publication proves the
inferred escaping set is contained by the declared bound.

The complete callable type and published signature include:

- input and output stack rows;
- the ordered parameter list including borrow, mutable-borrow, take, or value mode and receiver mode;
- generic type/value/pack parameters, constraints, and selected evidence identities;
- the result and tuple/product shape;
- a capability/effect row;
- an escaping-exception contract;
- a suspension contract;
- linkage, symbol/mangling contract, calling convention, fixed versus C-variadic status, target ABI,
  and parameter/result ABI classifications where externally visible;
- optional `nothrow`, purity, totality, determinism, allocation, and numeric-overflow guarantees
  useful to callers and optimization.

Private callables infer suspension. A published callable chooses `non-suspending`, a `suspends`
upper-bound contract that permits compatible suspension to be added later, or `suspends infer` to
freeze its exact inferred suspension summary. Widening an explicit public contract from
non-suspending to suspending, or widening an exact inferred summary, is breaking because callers may
hold borrows or rely on checkpoint boundaries. These properties participate in callable
substitution, module interface hashes, native cache keys, evidence-slot compatibility, and callback
validation; a throwing, suspending, taking, or foreign-ABI callable cannot be stored behind a
narrower callable type merely because its value parameters and result match.

Declaration attributes are separate compile-time metadata, uniformly spelled `@name(...)` for both
core and user definitions. Core attributes live in explicit namespaces and may be imported into
short names; user attributes have the same typed reflection and bounded `syntax -> syntax`
transformation contract. Callable inputs/outputs and `! EffectRow` are type structure rather than
attributes. There is no D-style mixture of magic bare attributes and second-class user annotations.
Reflection derives `pure`, `total`, `nothrow`, `deterministic`, and `non-suspending` as separate
properties. They are ordinary callable constraints, not `@` annotations: an inferred exceptional
exit may satisfy `pure` while failing `total` and `nothrow`, and `yields<Y,Resume>` prevents
non-suspending transformations.

Capability requirements should likewise become ordinary versioned typed descriptors rather than a
permanent closed compiler enum. A host registry supplies the stable unforgeable capability identity,
selector schema, containment relation, and implementation binding. User modules may define abstract
effects, handlers, wrappers, attributes, and attenuation, but declaring the same textual name never
mints host authority. This lets `fs.read(R)` be reflected, structurally matched, and wrapped as typed
effect-row data while preserving the broker as the authority boundary.

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
a `fiber<Y,Resume,R>` exposes producer progress, typed resumption, and a terminal result, and an agent is a separate
ProgramRun with its own authority and provider protocol. No construct shares a parent operand
stack or Rust thread/channel handle merely because it shares lifecycle machinery.

### Fibers, streams, deferred work, and repeated yields

`task<T>` remains the existing opaque scheduler handle. Its `join` operation is terminal: it may
suspend internally and returns one final `T`. A lazy `stream<T>` is the simpler multi-value
abstraction; it owns a cursor
and advances only when its consumer asks for the next value:

```text
stream-next stream : option<T>  ; bounded pull; none means exhausted
stream-close-discarding stream : unit ; cancel producer and drop unread values
```

The semantic primitive is a **private resumable execution**, not a generator or scheduler policy:

```text
ResumableExecution<Y,Resume,R> = verified frames + private operand stack + locals/captures + PC
                                 + transaction/effect prefix + lifecycle state
```

A coroutine function may create an instance of that state. Suspension propagates normally through
the entire ordinary call chain until some boundary handles or reifies it. A generator is the
`Resume = unit` pull contract; a fiber is an owned handle plus explicit call/yield policy; a green
thread, event-loop task, actor, or compiler semantic job is a scheduling/event policy over the same
instance. None gets an independently implemented continuation format. Creating an independently
resumable instance always starts a private stack from explicit arguments and immutable captures;
normal calls remain the way to operate on the current stack. Shared `cell<T>`, atomics, mutexes, or
channels are separate explicit memory resources, not implicit fiber communication.

Ordinary callers do not acquire `async`/`await` coloring merely because a callee can park on I/O.
For example, a database call may suspend the current ProgramRun internally and later return its
ordinary value; `MaySuspend` is inferred and verified like the other effects. Explicit concurrency
syntax appears only where the programmer creates or takes ownership of concurrent work, such as
`spawn`, `join-all`, `race`, or cancellation. A caller must not need to know whether an ordinary
callee parked, exhausted a scheduling quantum, or completed without suspension.

The runtime expresses that separation with a private discriminated drive step. It must not encode
all suspension as an ambiguous `yielded` state, and every nonterminal step carries the sole
unforgeable lease for continuing the execution:

```text
DriveProgress<Y> =
    SemanticYield { event_id, value: Y }
  | Parked { wait_id }                         ; host/I/O wait; scheduler-private
  | Runnable { reason: YieldNow | Fuel }       ; scheduler-private

DriveStep<Y,R> =
    Continued { progress: DriveProgress<Y>, successor: ExecutionLease }
  | Terminal { event_id, outcome: Terminal<R> }

Terminal<R> = Returned(R) | Thrown(ExceptionTransfer)
            | Cancelled(Diagnostic) | Trapped(Diagnostic)
```

Only `SemanticYield` and `Terminal` may project through a producer-facing policy API. `Parked` and
`Runnable` are scheduling facts, not values a programmer must interpret. A direct owner receives
the successor lease; after scheduler registration the registry retains that lease atomically and
publishes only the policy projection. A consuming drive step therefore either returns/retains the
sole successor or records one terminal outcome; an exception, trap, or cancellation can never lose
the execution between those actions.

```text
PolicyEvent<Source,Y,R> =
    Yielded { source: Source, event_id, value: Y }
  | Completed { source: Source, event_id, outcome: PolicyTerminal<R> }

PolicyTerminal<R> = Returned(R) | Thrown(ExceptionTransfer) | Cancelled(Diagnostic)
```

Single-source wrappers may omit the source field, while multi-source combinators retain it. A
runtime `Trapped` outcome never becomes an ordinary policy value: the policy first performs its
ownership-safe sibling cleanup, then preserves the non-catchable trap edge.

The general resumable handle uses linear typestates. Generator, coroutine, fiber, thread, task, and
custom-scheduler APIs wrap these transitions rather than defining new continuation representations:

```text
ready-fiber<Y,Resume,R>       dormant, not yet advanced
suspended-fiber<Y,Resume,R>   stopped at one yield
fiber-step<Y,Resume,R>        yielded(Y, suspended-fiber<Y,Resume,R>) | Done(R)
fiber-state<H,R>              pending(H,FiberStatus) | Done(R)

defer        : take closure -> ready-fiber<Y,Resume,R>        ; throws ResumableLimit
yield        : take Y -> Resume
fiber-start  : take ready-fiber<Y,Resume,R> -> fiber-step<Y,Resume,R>
fiber-resume : take suspended-fiber<Y,Resume,R>, take Resume -> fiber-step<Y,Resume,R>
fiber-next   : take ready-or-suspended<Y,unit,R> -> fiber-step<Y,unit,R>
done-value   : take Done<R> -> R                              ; ordinary library unwrap
fiber-cancel : take ready-or-suspended<Y,Resume,R> -> unit    ; throws CleanupFailure
fiber-try-join : take dynamic-handle<R> -> fiber-state<dynamic-handle<R>,R>
```

Here `take` is a parameter mode in the illustrative signature, not a mandatory token at every call
site. Parameters borrow by default. Supplying a unique/affine value to a taking parameter moves it;
an eligible copy/shared owner uses its ordinary copy/retain operation so the caller remains valid,
unless the caller explicitly chooses `move` to transfer that existing handle instead.

`defer` reserves the scheduler-registry/reaper accounting described below and creates a dormant
ready handle without running it in the background. It is non-suspending and throws the published
`ResumableLimit` value if capacity is unavailable; a scheduler policy may separately offer an
awaiting admission operation. `fiber-cancel` similarly publishes `CleanupFailure` as its stable
upper bound. Exceptions from the resumed callable itself remain inferred through start/resume/next
like ordinary calls. Each advance consumes the previous handle and returns either the sole next
suspended handle with its yielded value or ordinary standard-library `Done<R>`. The caller must
pattern-match that result.
Only `Done<R>` is accepted by `done-value`, so safe statically typed code cannot pass it incomplete
work. `Done` is no more compiler-special than `Result`: its library operation simply unwraps
`R`. Constructing another `Done(value)` cannot forge a continuation because the VM's ready/suspended
handle has already been consumed separately. An erased/runtime `try-join` consumes its handle and
returns either `Done<R>` or `pending(handle,status)`, preserving the sole handle on an incomplete
path. It never advances, waits, copies a unique `R`, or throws merely because work is incomplete.

Reaching a yield is progress, not proof that the producer consumed all input or reached terminal
`R`; no operation silently advances while discarding `Y`. A caller explicitly loops over the
returned handle, uses a `collect`/`fold` policy that accounts for every yield, or transfers the handle
plus a typed yield-to-resume policy into a scheduler. Yield and resume cross private-stack ownership
boundaries: non-copyable values move, copyable/shared values use ordinary copy/retain evidence, and
an escaping borrow is rejected.

Programmer-facing handles are policy-specific wrappers, not aliases for a raw fiber handle:

| Wrapper | Programmer-visible operations | Meaning |
|---|---|---|
| `Task<R>` | `join`, `join-all`, `race`, `select-complete`, `cancel` | one terminal result; internal suspension is invisible |
| `Generator<Y,R>` | `next`, `collect`, `fold`, `close-discarding` | pull exactly one semantic yield at a time, then terminal `R` |
| `BufferedProducer<Y,R>` | `next`, `next-any`, `merge`, `close-discarding` | scheduler-owned bounded queue of semantic yields |
| `Coroutine<Y,Resume,R>` | `start`, `resume`, `cancel` | start needs no reply; each later semantic yield requires a typed reply |
| `GreenThread<R>` | task-style `join`, `cancel` | scheduler owns cooperative advancement |

The type determines the valid coordination vocabulary. `next task`, `join generator`, or
`next-any` over a bidirectional coroutine is rejected at compile time with a diagnostic naming the
required wrapper or explicit adapter. Libraries may define new policies through concepts, but must
map their public events and ownership transitions explicitly. This is a usability invariant: if a
caller needs implementation knowledge of a producer's private yields, parking, or scheduling to use
its handle correctly, the policy interface has failed.

The standard task combinators have deterministic ownership and failure contracts. Their conceptual
caller-facing signatures are:

```text
join            : take Task<R> -> R
join-all        : take Tasks<Results> -> Results
cancel-on-error : take Tasks<Results> -> Results
race            : take HomogeneousTasks<Source,R> -> (Source,R)
select-complete : take HomogeneousTasks<Source,R>
                  -> (Completed<Source,R>, RemainingTasks<Source,R>)
```

These operations may suspend internally without source-level `await`. `Returned(R)` supplies the
value, `Thrown(E)` rethrows the typed value with its original provenance, `Cancelled` throws the
published `TaskCancelled` value, and `Trapped` remains a non-catchable runtime trap. Aggregate
operations finish their stated sibling cleanup before propagating the selected primary terminal
outcome. The explicit `select-complete` result retains source-visible `PolicyTerminal<R>` so
orchestration code can inspect a returned, thrown, or cancelled completion rather than immediately
propagate it; a runtime trap remains a trap after cleanup.

- `join-all` consumes every task, waits for every terminal outcome and returns results in input
  order. A heterogeneous fixed input produces a typed tuple; a homogeneous collection produces a
  collection. If several tasks do not return, the lowest input position is primary and later
  positions become ordered suppressed diagnostics, independent of completion timing.
- `cancel-on-error` records the first journaled failure as primary, requests cancellation of the
  remaining tasks, awaits their cleanup, and retains later failures as ordered suppressed
  diagnostics.
- `race` selects the first terminal outcome, never the first internal park or producer yield, then
  cancels and reaps every loser before releasing their ownership. A later `race-success` may ignore
  failures until every candidate fails, but is a distinct operation.
- `select-complete` returns the next terminal event together with the remaining tasks under one
  linear composite owner, so unselected handles cannot be lost or advanced concurrently.
- `next-any` initially requires homogeneous `Y` and `R` and returns the next semantic yield or
  terminal event from a set of unit-resume buffered producers. Its result identifies the source and
  stable event ID. Heterogeneous tuples require an explicit tagged sum. Bidirectional coroutines
  require an explicit response policy and are not accepted initially.
- `merge` initially requires homogeneous `Y`, `Resume = unit`, and `R = unit`. Heterogeneous sources
  use an explicit tagged variant rather than runtime guessing.

Simultaneous readiness uses a stable journaled tie-break and a rotating fairness cursor, so replay
chooses the same winner without permanently favoring the first input. `race` and cancellation do
not imply rollback of host effects already recorded by a losing task.

Durable composite policies checkpoint child identities and generations, queue contents and retained
bytes, pending typed resumes, observed terminal outcomes, fairness cursor/epoch, selected winner,
cancellation-cleanup phase, and delivery/acknowledgement state. Restart restores that ownership
record before any child advances, preventing a replay from selecting a different winner, repeating
an acknowledged value, or abandoning an unselected child.

For `Resume = unit`, a scheduler policy may buffer moved `Y` values in a bounded per-producer queue
and immediately continue the producer while item, retained-byte, fairness, and fuel limits permit.
This avoids a rendezvous and wakeup for every value. A full queue applies backpressure by suspending
that producer; an item larger than the byte allowance fails with a structured resource-limit value
rather than waiting forever. For non-unit `Resume`, every semantic yield suspends until its typed
reply arrives. `yield-now` is separate: it ends the current scheduling quantum and produces
`Runnable`, never a semantic item.

Accepted semantic yields remain ordered before that producer's terminal event. Normal consumption
drains already-accepted values before observing failure. There is no ambiguously named `close` on a
producer wrapper: explicit `close-discarding` cancels and reaps the producer while dropping queued
values through their ordinary destructors. Within one VM transaction,
delivery and acknowledgement are exactly once. Across a non-transactional external boundary,
delivery is at least once with the stable event ID until acknowledged. Single-threaded schedulers
may use ordinary deques without locks; cross-worker implementations use per-producer or per-worker
queues and batched notifications rather than a contended global rendezvous.

The shared primitive supports distinct policies without conflating them:

| Policy | Progress owner | Yield/resume contract | Completion |
|---|---|---|---|
| generator | calling consumer | semantic `Y` / `unit` | match `Done`, then `done-value` |
| coroutine | calling peer | semantic `Y` / typed `Resume` | match `Done`, then `done-value` |
| green thread | cooperative scheduler | drive events remain scheduler-private | `join` its typed task surface |
| event-loop task | event-loop scheduler | host waits remain scheduler-private | `join` its typed task surface |
| custom fiber scheduler | declared policy implementation | declared `Y` / `Resume` | the same `Done<R>` terminal value |

These policies belong in the standard library wherever semantics permit. The compiler/runtime kernel
owns only operations that ordinary code cannot safely synthesize: capture verified frames, suspend
with typed `Y`, resume with typed `Resume`, cancel/unwind the private execution, and mint unforgeable
linear ready/suspended handles whose terminal transition consumes the handle and returns `R`. The
standard library defines `Done`, `Task`, `Generator`, `BufferedProducer`, `Coroutine`, green-thread
adapters, task/producer combinators, collection/fold helpers, and scheduler policy concepts as
ordinary parameterized types with explicit mappings to those intrinsics. User schedulers may
implement the same concepts. Selective specialization and JIT inlining follow resolved evidence and
IR behavior rather than privileged standard-library type names.

A scheduler policy consumes the direct handle and becomes its only progress owner. It returns a
task/observer surface appropriate to that policy; the original binding is unavailable, so two
callers cannot race to advance it. A `poll` returns status-only `pending`/`complete` or a borrowed
terminal view; it never moves an owned `R` from a borrowed task.
`join` consumes `Task<R>` and may suspend internally until the scheduler produces
terminal `R`; ordinary callers do not spell `await`. `done-value` unwraps an already-produced
`Done<R>` from direct generator/coroutine advancement. Custom policies map yielded values to resume
decisions through explicit concept evidence and cannot inspect or forge private
continuation frames. OS worker threads are merely one execution policy for verified resumable state
and require ordinary cross-worker transfer evidence.

For a compiler semantic fiber, the scheduler consumes every `CompilerNeed`, advances the dependency,
and supplies one `CompilerResolution`; none of those values are presentation-only progress that may
be dropped. If an advance reaches a host await, the owning ProgramRun suspends through the normal
typed effect/resume path rather than blocking an OS thread. A thrown failure propagates only after
fiber and coordinator cleanup obligations run.

Dropping an unfinished affine handle does not synchronously unwind it: the handle's nonthrowing,
non-suspending drop atomically transfers the owned execution and cleanup stack to a scheduler reaper
in `CancelRequested` state. Creation of resumable execution reserves an entry and bounded cleanup
accounting in the owning ProgramRun's scheduler registry; creation backpressures or fails with a
structured resource-limit value before that quota is exhausted. Drop therefore marks and transfers
an already-accounted record without allocating or growing an unbounded queue. The reaper drives
cancellation with fair scheduling and per-origin limits, and records exactly one terminal outcome.
It has reserved cleanup fuel/time/await allowances independent of exhausted user work budgets so
ordinary cancellation can finish. If a suspending guard exceeds those bounds, the runtime records a
suppressed cleanup/resource-limit diagnostic, continues mandatory non-suspending drops, and
terminalizes the record rather than retrying forever. An embedder must drain or durably hand off the
owned registry before clean shutdown; hard process abort retains the ordinary no-cleanup guarantee.
Explicit
`fiber-cancel` is stronger: it consumes the handle and cooperatively suspends as needed until cleanup
is terminal, then returns `unit` or propagates the structured cleanup diagnostic.

Scheduler ancestry detects self-wait and dependency cycles instead of waiting forever. Before
reporting a cycle, it terminalizes the affected scheduler-owned jobs, unwinds each execution exactly
once, and releases their handles; diagnosed work is never stranded in the registry.

The source program never writes a continuation. A fiber `yield value` may occur any number of
times; the VM records remaining frames as an internal resumable-execution record and advances it
through the current owner. `defer` reifies/transfers ownership of that record into a handle; it must
not clone or reconstruct generator semantics in a second scheduler implementation.
This uses the same typed `yield` instruction as ordinary ProgramRuns, not a second fiber-only
primitive: its function/fiber contract declares `Y` and the resume value, and
the scheduler records both in the same typed suspension record used by every `MaySuspend` word.
Callable signatures and first-class closure types retain this as `yields<Y,unit>` metadata. The
frontends infer it transitively from direct yields and calls, while the independent verifier derives
it again from IR and rejects a function that hides or changes its suspension contract.
The general form is `yield : Y -> Resume`, recorded as `yields<Y,Resume>` on every suspending
callable. VM storage may use the ordinary tagged `TypedValue` representation, but source-facing
wrappers statically establish `Y` and `Resume`; a raw boxed escape hatch must never make routine
generator/coroutine code `dynamic`. `Resume = unit` is the present generator profile. A typed
`require(name, phase) -> Symbol` can later yield a structured compiler request and receive the
resolved symbol through the same channel. `defer`, `next`, `resume`, scheduler registration,
and event-loop handling remain ordinary generated vocabulary/library policies over one runtime
record, not privileged multi-return conventions. A resumable producer can be adapted to the
future effectful stream/range concepts through visible library code; it is not the definition of a
pure synchronous range.

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

### Future ranges, cursors, and explicit erasure

Ranges are a future source-language/library facility, not a compatibility constraint on the
current scheduler-owned `stream<T>` handle. Keep a pure synchronous `Range`/`Cursor` family
separate from `Stream` or `AsyncRange`, whose advancement may suspend, fail, consume a resource, or
perform host effects. A range adaptor should be an ordinary composed value; it need not allocate a
producer, own a scheduler record, or erase its concrete type merely because the type is inconvenient
to spell.

Parsing over a range must make consumption explicit. A prefix parser returns the parsed value plus
the remaining range/cursor; it does not claim whole-document success. A document parser consumes
permitted trailing whitespace and proves end-of-input before returning success, otherwise it reports
the first trailing token with its span. Dropping a range or observing one syntactically complete
value never implies that unconsumed input was accepted.

An adaptor may therefore return an opaque existential such as `some Range<Item=T>`. This
"Voldemort type" hides the private concrete adaptor name from source and module consumers while
retaining that one concrete type and its static evidence for optimization. It is distinct from
`dyn Range<Item=T>`, which intentionally erases the concrete type and dispatches through runtime
evidence. Runtime factories are a primary reason to return `dyn Range<Item=T>` when the selected
range implementation depends on configuration, a plugin, or another runtime choice.

Start with the smallest useful contracts and add refinements only when algorithms can exploit
them: forward traversal first, then bidirectional, random-access, sized, or contiguous guarantees
as independently stated concepts. An adaptor explicitly derives the evidence it preserves. For
example, `map` may preserve sizing and traversal direction but not contiguity; `filter` may preserve
forward traversal but not exact size. These derivations use ordinary concept rules and named
operation mappings, not forests of D-style `static if`, `is(...)`, or
`__traits(compiles)` probes. Here a range "capability" means refinement evidence, never a Finch
host capability or authority grant.

Adaptor pipelines can create deeply nested static types and excessive specialization even when no
individual adaptor is expensive. Provide an explicit erasure/materialization checkpoint that turns
such a pipeline into a chosen collection, shared cursor representation, or `dyn Range`. Erasure is
visible in the type and dispatch model; the compiler must not introduce it silently merely to make
type growth or compilation cost disappear.

### No privileged collection or iteration overloads

Surface convenience must never create a standard-library-only fast path. A future `for`/`foreach`
form may be compiler-owned syntax that selects an indexed loop, synchronous range loop, effectful
stream pull loop, or collection-specific loop during lowering. Each selection must be justified by
public concept evidence for the required cursor operations and refinements. A user-defined range
maps those operations to the same stable word identities as a built-in range. The optimizer may
inline, specialize, fuse, or eliminate allocations after that resolution, but it may not recognize
only `list`, `map`, or a compiler-owned iterator type while treating equivalent user evidence as
dynamic dispatch. A user-written `foreach`, traversal, or adaptor must remain eligible for the same
optimizations as syntax supplied by Finch.
There is one staged `foreach`, not a separate `static foreach`: when its range and pure body are
compile-time values, bounded CTFE executes it; when the range is a runtime value, lowering emits the
ordinary verified range loop. Partial evaluation may specialize known structure and leave residual
runtime code, using the same public contracts in either stage.

The exception is the deliberately small execution substrate: verified branch/suspend instructions,
managed allocation, and authorized host calls. Those are represented by public typed words and
their contracts in the registry; user source cannot manufacture arbitrary IR or host authority.
Everything above that substrate—including collection algorithms and range iteration—remains
ordinary vocabulary that can be inspected, replaced, composed, and optimized.

### Capability effects are authority requirements

A type describes values. The canonical effect row describes observable semantic behavior, including
mutation, suspension, nondeterminism, and capability-bearing host operations. A capability effect is
the subset that requests authority; the broker ignores non-authority members while the verifier,
scheduler, and optimizer consume the members relevant to them. For authority, the target replaces a
single ordered `ExecutionEffect` with a set of parameterized requirements:

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

Implementations may index or cache row members by kind for fast consumers without exposing parallel
annotation systems or changing the single source-level union algebra.

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
: square ( S consume-value int -- S int ) guarantees pure
  dup *
;

: save-report
  ( S borrow path<workspace:"generated/**"> value string
    -- S path<workspace:"generated/**"> unit
    ! {fs.write(workspace:"generated/**")} ) suspends
  file.write
;
```

Signatures are compiler-readable, not comments. A compatibility reader may initially accept classic
`( ... )` comments, but verified definitions store a parsed `Signature` object.

### Canonical structured surface and parity ledger

Co-Forth is not merely the low-level subset of CoLisp. Its postfix evaluation order maps closely to
typed stack IR, but every common semantic AST node has a direct structured spelling. The following
tokens are the target canonical forms; convenience aliases may be added only when they parse into
the same nodes without source-to-source CoLisp generation.

| Semantic form | CoLisp | Co-Forth | Semantic construction / IR family |
|---|---|---|---|
| module identity | `(module name ...)` | `module: name ... ;` | module declaration, no runtime instruction |
| immutable import/export | lexical `(import ref ...)`, `(from ref :import ...)`, `(export ...)` | lexical `import: ref ;`, `from: ref import{ ... } ;`, `export: ... ;` | scoped import declaration and resolved module/symbol identity |
| record/layout | `(record Foo ...)` | `record: Foo repr(...) fields{ ... } ;` | record schema/layout |
| record construction/projection | `(Foo :x a :y b)`, `(. value x)` | `Foo{ x: a y: b }`, `value .x` | `RecordNew`, `FieldGet`/borrow projection |
| closed variant | `(variant Result ...)`, `(ok value)` | `variant: Result cases{ ... } ;`, `value Ok{}` | `VariantNew` |
| pattern match | `(match value ...)` | `value match ... of ... endof endmatch` | typed decision tree / `Match` |
| generic declaration/application | generic header, explicit type application | `< types T... values N... > ... where ...`, `word<...>` | parametric artifact / fixed call |
| parameter/rest pack | delimited call or `:spread` | `args{ ... }`, `rest{ ... }`, `rest-spread` | fixed operands or one rest collection |
| text/collection literal | `string`, `array`, `vector`, `list`, `bytes` forms | string literal, `array{}`, `vector{}`, `list{}`, `bytes{}` | literal node plus public builder evidence |
| view/index/slice | borrow, `.get`, index/slice forms | `borrow`, `.get`, `index`, `slice` words | borrow projection and checked access |
| text traversal | `.bytes`, `.chars`, `.graphemes` | `text-bytes`, `text-chars`, `text-graphemes` | explicit range evidence |
| operators/comparison | `(== a b)`, `(+ a b)`, explicit `using` | `a b ==`, `a b +`, explicit `using` | named binary-concept evidence call |
| builder/freeze | builder operations and `freeze` | `*-builder`, mutation words, `freeze` | unique owner mutation then consuming conversion |
| concept/evidence | `concept`, `implementation`, `using` | `concept:`, `implementation:`, `using` | named evidence and adapter thunk |
| dispatch type/view | `static C`, `dyn C`, `some C` | same type constructors; `as-static`, `as-dyn`, `as-some` words | evidence constant, erased view, opaque result |
| exception region | `(try body (catch ...))` | `try ... catch { error } ... endtry` | `HandlerEnter`/`HandlerExit` and match |
| exception transfer | `(throw e)`, `(rethrow e)` | `throw`, `rethrow` | `Throw`, `Rethrow` |
| scope guard | `(scope exit|success|failure action)` | quotation followed by `scope-exit`, `scope-success`, or `scope-failure` | lexical cleanup record |
| closure capture | `lambda` capture specification | quotation `captures:` header | `CaptureSpec`, `MakeClosure` |
| ownership | `new unique`, `new shared`, `share`, borrow/take/retain | `new-unique`, `new-shared`, `share`, `borrow`, `take`, `retain`, `weaken` | owner/lifecycle operations |
| fibers/tasks | `defer`, `spawn`, `join`, `race`, `next` | same typed words applied to quotations/handles | scheduled-execution operations |
| range iteration | range operations / `foreach` | range words and quotation `foreach` | concept calls and structured loop |
| named tests/suites | `(test ...)`, `(test-suite ...)` | `test: ... {}`, `test-suite: ... {}` | test-profile declarations, no production instruction |
| macro/syntax | `define-syntax`, syntax constructors | `macro:`, `syntax[ ... ]`, explicit splice/fresh/context words | `Syntax` CTFE, then ordinary nodes |
| unsafe/FFI | `(unsafe ...)`, `(extern "C" ...)` | `unsafe[ ... ]`, `extern(C): ... ;` | marked unsafe/foreign call; unhosted only |

Structured delimiters such as `Foo{...}`, `args{...}`, `match...endmatch`, and `unsafe[...]` are
reader forms that retain spans and nesting; they are not ordinary words searching backward through
an unbounded ambient stack. Record construction is field-labelled, and variant/pattern fields are
explicit, so layout changes cannot silently reinterpret positional source.

Representative forms are:

```forth
module: reports.user
import: codec.json@sha256:... { JsonSerializable UserJson } ;

record: User repr(native) fields{
  id: int
  name: string
} ;

variant: ParseResult cases{
  Ok(Date)
  Error(ParseError)
} ;

: show-result ( S borrow ParseResult -- S string )
  match
    Ok{ date } of date format-date endof
    Error{ error } of error describe endof
  endmatch
;

: load-user ( S borrow path -- S Config ) throws ConfigError suspends
  try
    file.read parse-config
  catch { error }
    error match
      ConfigError.NotFound{ path } of path default-config endof
      remaining of remaining rethrow endof
    endmatch
  endtry
;
```

The full grammar must assign every delimiter, keyword, and nesting rule unambiguously and reserve
them before the feature ships. A feature is parity-complete only when the specification contains
canonical source in both syntaxes, both construct the same semantic node family, successful cases
produce equivalent typed IR and results, and their principal invalid case produces the same stable
diagnostic with frontend-specific source spans. Equivalent semantics do not require identical sugar.

### Locals, quotations, and closures

Provide explicit locals for generated code and readable handwritten definitions:

```forth
: hypotenuse { x:int y:int -- float }
  x x * y y * + int>float sqrt
;
```

Quotations are typed callable values:

```forth
[ consume-value int -- int guarantees pure | 1 + ]
```

An escaping quotation is closure-converted into an immutable code reference plus an owner-carrying
captured environment. Calls use `call`/`tail-call`; they do not create an untyped anonymous stack.

### Closure conversion and capture ownership

Closure conversion is a concrete lowering pass, not a second evaluator. The frontend resolves each
free lexical name to the nearest binding, orders captures deterministically by resolved binding
identity, emits the capture operations in that order, and emits `MakeClosure(function,
capture_count, signature)`. The generated function has a typed capture vector and reads it only
through `CaptureGet`; parameters become frame locals in normal call order. A closure therefore
captures a proven scoped borrow or an owner, never an unchecked alias to a caller operand stack,
mutable frame, grant, or ambient host authority.

Lambda capture policy is an optional structured operand of the lambda form, not a runtime
parameter. Illustrative canonical CoLisp forms are:

```lisp
(lambda (x) body...)                         ; inferred minimal captures
(lambda :move (x) body...)                   ; used free bindings captured by value
(lambda (:captures (borrow config)
                   (take socket)
                   (retain cache))
        (x)
        body...)                              ; exact capture contract
(lambda (:captures :move (borrow config))
        ()
        body...)                              ; value default with an override
```

The corresponding Co-Forth quotation header is structured syntax before `|`, not values executed
on the operand stack:

```forth
[ ( S consume-value int -- S int ) | ... ]
[ captures: move ( S consume-value int -- S int ) | ... ]
[ captures: {
    borrow config
    take socket
    retain cache
  }
  ( S value Request -- S Response )
| ... ]
[ captures: move { borrow config } ( S -- S unit ) | ... ]
```

The reader produces the same `CaptureSpec` and ordered `CaptureEntry` syntax objects as CoLisp.

With no policy, the compiler may infer scoped borrows for a closure proven not to escape or
suspend. If such a closure is returned, stored, deferred, dynamically erased, passed to an unknown
callee, or crosses suspension, compilation fails at that boundary and suggests `:move`, an explicit
owning capture, or a `scoped` callback contract. `:move` captures each used free binding by value
according to its existing type: `Copy` values copy, unique owners and other non-copyable values
move, `Shared<T>` retains/copies its handle, and moving an existing borrow moves only that borrow
without acquiring its referent. An exact `:captures` list rejects unlisted free bindings. Capture
entries may explicitly borrow, mutably borrow, take, retain, weaken, clone, or bind a computed
expression under a capture name; each operation uses its ordinary ownership and effect contract.

The compiler materializes captures conceptually as one anonymous record plus a code identity. That
record is the callable's hidden receiver: readonly invocation borrows it, mutation of captured
fields requires an exclusive receiver, and consuming an owned capture requires a taking receiver.
The corresponding callable evidence is generated from those operations. A `self` referenced by a
lambda inside an operation is an ordinary capture, not a second privileged context pointer.
Lexically nested record declarations are context-free and never gain a hidden outer-object or
enclosing-frame field merely because of their declaration location; required context must be an
explicit field or closure capture.

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
  make-closure lambda$0 captures=1 : (S consume-value int -- S int) guarantees pure
  const.int 5
  call-closure (S consume-value int -- S int) guarantees pure
  return

lambda$0 captures: [int], locals: [int] # n is capture[0], x is local[0]
  local.set 0                 # pop x from the callee's private operand window
  local.get 0
  capture.get 0
  call core.add
  return
```

The target runtime, once the ownership model is implemented, consumes or borrows the closure
according to its call signature, creates a fresh frame
with a private operand window above the caller boundary, projects its captures into that frame, and
destroys the frame on return. Only the signature-declared results cross back into the caller window.
This is also why `(defer :cpu (lambda () ...))` is safe: it moves unique captures and retains shared
captures into a separate CPU task and never shares a borrowed parent stack location.

**Target representation and allocation rule.** Copyable primitive captures (`int`, `bool`,
`float`, `char`, symbols, small opaque handles) are copied inline in the closure value. An explicit
or inferred borrow remains a scoped borrow; an owning capture stores its actual inline, unique,
shared, weak, or user-defined carrier. The initial interpreter may represent a short-lived closure
as an owned `TypedValue::Closure` and needs no tracing heap. Escape analysis may stack-allocate,
inline, or eliminate a closure environment when that is unobservable, but it never changes what
the source capture policy captured. A closure is conservatively escaping when it is returned,
stored in a collection/record/dictionary, placed on the persistent VM stack, passed through
`dynamic`, used by `defer`, or handed to a host or unknown call boundary.

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
on a caller stack. `for` may be added only as a bounded desugaring to these loop blocks.
`FINCH-LISP/1` currently uses `try` as result-propagation syntax; that remains implementation
history, not the target exception contract. A future version reserves `try` for a dynamically
scoped exception handler and uses ordinary matching/library operations for `result` values. The
version transition requires an explicit source migration rather than silently reinterpreting stored
`/1` programs. Target handlers follow the contract below and never catch authorization, replay,
cancellation, or verifier diagnostics indiscriminately.

### Typed failures and scope guards

Finch separates a failure value from the mechanism used to transport it. `option<T>` represents
ordinary absence and `result<T,E>` represents an expected domain alternative the program wants to
store, transform, return, or match. Both are library variants. `throw value` instead creates an
exceptional control-flow edge carrying an ordinary typed value toward the nearest dynamically
enclosing compatible handler. An `ExceptionValue` concept may map that value to a stable code and
human diagnostic, but there is no exception class hierarchy and conformance is never inferred from
member names.

`throw` must acquire an owned payload before unwinding. A unique value moves into the exception
transfer and disarms its former cleanup; an eligible copy/shared value is copied or retained under
its ordinary lifecycle evidence. A borrow may be thrown only when copying its referent produces an
independent owned value; otherwise the compiler rejects the escaping borrow. Exception transfers
that survive suspension, migration, or checkpointing obey the same ownership and serialization
rules as every other continuation value.

Calls propagate exceptional exits automatically. The compiler infers their possible value types
through the call graph, recursive strongly connected components, generics, and dynamic evidence; it
does not require source annotations or `result` plumbing in intermediary functions. Private and
recursive inference remains bounded and monotonic. Published module interfaces freeze the selected
exception contract for downstream checking. Widening that public bound is a breaking interface
change because exhaustive handlers and `nothrow` proofs must be rechecked; narrowing an
implementation's inferred set within an unchanged explicit bound is not. With `throws infer`, the
exact set is the contract, so adding an escaping type is breaking while removing one is compatible
but still changes the content/interface hash. A dynamic call without stronger evidence is
conservatively considered capable of throwing any value permitted by its published contract.

Exception sets and explicit bounds are canonical subtype antichains, not flat nominal lists. An
inferred exception `E` is covered by a published bound when some declared `D` satisfies `E <: D`.
The canonical form removes any member already covered by a broader member and orders the remainder
by stable type identity, so `{IoError, FileNotFound}` is `{IoError}` when
`FileNotFound <: IoError`. Interface hashing, widening checks, dynamic-call summaries, handler
subtraction, and exhaustiveness all operate on that same canonical form. Diagnostics may retain the
more specific inferred type and source origin even when its contract representation normalizes to a
broader bound.

`nothrow` is the explicit checked guarantee. A default callable may propagate an exception. A
`nothrow` callable may call such code, but every possible exceptional edge must be caught and
consumed before leaving the callable; a partial handler, rethrow, or throwing handler leaves an edge
and is diagnosed at the operation that introduced it. A `nothrow` callable is usable where a
possibly throwing callable is accepted, but not conversely. Copy and drop hooks are implicitly
`nothrow`: they may call throwing code internally only when an exhaustive handler consumes it.

The target Lisp handler syntax makes the dynamic extent visible while reusing ordinary patterns:

```lisp
(try
  (begin
    (load-config path)
    (initialize-services))
  (catch
    ((ConfigError.NotFound missing-path)
      (create-default missing-path))
    ((as error (ConfigError.InvalidSyntax problem))
      (log-with-context error)
      (recover problem))))
```

`try` installs its handler before evaluating the expression or `begin` block. Success bypasses the
handler. On throw, scopes between the throw point and handler boundary unwind first, then `catch`
pattern-matches the thrown value. Pattern fields bind normally; an identifier binds the whole value;
an `as` pattern binds both the whole value and its narrowed fields. `catch` is shorthand for the
fully explicit `(catch error (match error ...))` form, plus a compiler-supplied final arm that
propagates an unmatched transfer, rather than a second matching engine. The selected handler is
inactive while its arm executes, so `rethrow` or a newly thrown value targets an enclosing handler
instead of recursively entering the same catch.
Handler selection only inspects or borrows the envelope and payload. It does not move payload fields
until an arm has been selected, so an unmatched catch can resume the original unwind with the same
owned value, type identity, and provenance intact.

After selection, ordinary move rules apply to bindings. An `as` pattern may borrow the whole value
while moving a field, but it cannot create two owners: moving a non-copyable field marks that portion
of the whole binding unavailable, and later whole-value use or `rethrow` is rejected unless the
pattern retained an independent owner. Unmoved initialized fields retain exactly-once cleanup.

An ordinary value `match` must be exhaustive. A catch matcher may be partial: an unmatched value
continues unwinding automatically without reboxing or losing provenance. Inside `nothrow`, inferred
exception types make effective catch coverage exhaustive. All successful and handled branches must
produce compatible values; an arm that explicitly rethrows has bottom type `never` and is compatible
with every result type.

Disjoint constructor patterns may appear in any order. If patterns overlap and one strictly
subsumes another, the more specific pattern must appear first; a broader-before-narrower or fully
shadowed arm is a compile error with both spans. Overlapping incomparable patterns are ambiguous and
must be rewritten. Structural patterns, declared subtype edges, and immutable variance determine
specificity; arbitrary predicate guards do not. The compiler lowers the checked matrix to a bounded
decision tree, memoizes repeated submatrices, and diagnoses rather than expanding a pathological
open pattern set without limit. Runtime matching therefore remains tag/type tests and branches, not
sequential reflection or dynamic duck typing.

Explicit library/macro operations may convert between the two policies at the point intent changes:
`result-or-throw(result)` throws its `err` payload, while `attempt(expression)` catches a declared
set and returns `result<T,E>`. Mapping a result remains useful for genuine value transformation, but
is not required merely to propagate failure or attach call context.

Cancellation uses the same internal unwind machinery but is not catchable by ordinary source unless
a future explicitly privileged control API requires it. Authorization outcomes and verifier/runtime
traps remain protected diagnostics, not convenient exceptions for source to swallow.

Every execution frame may retain lexical guard records containing a trigger (`exit`, `success`, or
`failure`), a typed closure, source origin, and once-only state. Guards run in reverse registration
order when that lexical scope actually exits. Yielding, awaiting a host effect, migration between
workers, or serializing a continuation does not fire them; the continuation carries them until
normal return, exception propagation, cancellation, or trap unwinds the scope. Explicit dismissal
supports commit-style compensation. Guards may invoke separately authorized compensating effects,
but they cannot erase an execute-once journal entry or claim that an external mutation rolled back.
All intervening guards and drops finish before a thrown value enters its matching catch arm. If a
guard throws while an exception, cancellation, trap, authorization failure, or resource-limit
termination is active, retain that protected outcome as primary, attach the guard failure as
suppressed, and continue unwinding rather than losing or replacing the cause. The suppressed value
is diagnostic context and cannot become an ordinary catchable replacement for the protected
outcome. A guard throw on a previously successful exit becomes the primary exception and changes
the remaining cleanup reason to failure.

Implicit drops and explicit guards occupy one lexical cleanup stack. Constructing an owned value
registers its drop at that point; registering a later guard places that guard above the drop. Scope
exit runs eligible cleanup records in reverse registration order, so a guard may use values that
were alive when it was registered and every owned value is still destroyed exactly once. Suspension
preserves this stack without running it. Drop itself is `nothrow`, cannot suspend, and cannot replace
an active diagnostic; its implementation may call throwing code only behind an exhaustive internal
handler.
A move transfers the source's cleanup obligation to the destination and disarms the source record;
it never registers a second drop for the same owner. Partially initialized records drop only their
initialized fields, in reverse field-initialization order. A guard that borrows a value holds a
checked loan until it runs or is dismissed, preventing that owner from moving or dropping first; an
escaping guard must instead move or retain its captures under the ordinary closure rules.

Trigger meanings are exact: `exit` runs for every structured scope exit; `success` runs only for a
normal value/return edge; `failure` runs for a thrown exception, cancellation, or defined runtime
trap. Returning `err(E)` is an ordinary value edge and does not trigger failure unless source
explicitly throws it. A throw caught entirely inside the same lexical scope does not exit that scope
and therefore does not fire its guards; guards in inner scopes unwound on the way to the handler do
fire before catch matching begins.

CoLisp spells these `(scope exit cleanup)`, `(scope success publish)`, and
`(scope failure compensate)`. Co-Forth spells them `[ cleanup ] scope-exit`,
`[ publish ] scope-success`, and `[ compensate ] scope-failure`; a dismissible registration returns
an affine guard token consumed by `guard-dismiss`. All three lower to the same IR cleanup record
rather than separate language features.

Guards are expressible as nested `try`/`finally` behavior but should not be implemented by repeatedly
rewriting the source AST. Elaboration registers one cleanup action and trigger at its lexical point;
CFG construction shares cleanup blocks across return, throw, cancellation, and trap edges. A
statically registered non-escaping guard normally needs no heap closure or dynamic handler record.
Conditional registration/dismissal requires only a local active bit, and runtime work occurs at
scope exit. Compilation and IR growth should be linear in lexical guards; the compiler must share
equivalent cleanup suffixes rather than duplicating every nested exit path.

### Dynamic and unsafe boundaries

Reflection and legacy words may be retained behind explicit boundaries:

```text
dynamic.call       requires runtime signature check
unsafe.memory      unhosted native profile only; hosted Finch rejects the module
legacy.eval        unclassified; interpreted only; explicit approval
```

Unsafe/dynamic words cannot be silently inlined into a verified pure definition.

## Shared type, ownership, and lifetime model

This section is a normative target for both source syntaxes. CoLisp and Co-Forth must be able to
state every rule below and must lower equivalent programs to equivalent ownership-bearing typed IR.
The model separates three questions that class hierarchies and many smart-pointer APIs conflate:
whether a call borrows or takes a value, which object owns its storage, and whether behavior uses
static or dynamic dispatch.

### Borrowing and taking

An ordinary parameter borrows for the invocation. A taking parameter receives ownership and may
store, return, destroy, or transfer the value beyond that invocation. `take` grants permission to
escape; it does not promise that the callee will store the value and does not itself select stack,
heap, unique, or reference-counted storage. Illustrative syntax is:

```text
inspect(x: Foo)                         # borrow; x cannot escape the invocation
retain(take x: static Owner<Foo>)       # take an owner; x may escape
retain-open(take x: dyn Owner<Foo>)     # same contract with an erased carrier
```

There is no hidden `take Foo` shorthand in the initial grammar. A taking signature states its
carrier dispatch so the parameter's representation is always knowable. Every ordinary owned value
`T` supplies intrinsic inline `Owner<T>` evidence through the lifecycle kernel; `Unique<T>`,
`Shared<T>`, and user-defined indirect carriers supply explicit implementations. In the static form
the hidden generic carrier type `O : Owner<T>` is inferred solely from the argument, the parameter
storage is `O`, and operations on `T` use its checked borrow projection. Storing or returning the
parameter stores or returns `O`, not an imaginary unwrapped `T`. The dynamic form receives the
declared erased envelope. Expected results never participate in this choice.

The call site does not need a ceremonial `move` marker when the parameter already says it takes:

```lisp
(begin
  (let foo (Foo ...))
  (retain foo)
  (inspect foo)) ; error: foo was moved by the preceding taking call
```

Passing a uniquely owned value to a taking parameter moves it and invalidates the source binding.
Passing a shared owner retains another strong handle, so the source remains usable. Passing either
kind to an ordinary parameter borrows through the owner without transferring or retaining it. An
explicit move of a shared handle may transfer that handle without incrementing its count and then
invalidates the source. Temporaries transfer directly because no source binding can be reused.

The transfer is unconditional from the caller's perspective. A callee that conditionally decides
not to retain a taken value still owns and must drop, return, or transfer it. It cannot make the
caller's moved state depend on a runtime branch. Control-flow joins track `available`, `borrowed`,
`exclusively borrowed`, and `moved` states; a value moved on only some incoming paths is not usable
after the join unless every path reinitializes it. A use-after-move is a compile error whose
diagnostic points both to the use and the taking call.

Readonly borrows may coexist. A mutable borrow is exclusive and temporarily prevents all use of its
owner. Borrows normally end at their last use, but the analysis is intraprocedural and directional:
the compiler does not solve general lifetime variables backward through callers. A borrowed value
cannot cross its proven owner boundary: it cannot be stored in an escaping value, returned without
a locally tracked input-owner relationship, captured by an escaping closure, placed in persistent
VM state, handed to an unknown host, or remain live across suspension. The ownership `borrow`
projection and range/item projections are narrowly defined borrowed results tied to exactly one
input receiver. Their origin remains in HIR, their use is checked within the caller, and they cannot
be erased, generalized, or exported as an unconstrained reference. Such an escaping boundary must
receive ownership instead.

### Library ownership carriers and the compiler lifecycle kernel

Unique ownership is the default for ordinary resource-bearing values. Copyability is an explicit
property of a type, not an assumption that every value may be duplicated. The compiler understands
only a small, general lifecycle kernel:

- definite initialization, moves, borrows, and last use;
- whether a type is movable, copyable, or immovable;
- non-suspending `nothrow` copy and drop hooks with declared effect rows;
- exactly-once reverse-order destruction on normal scope exit and structured unwind;
- the standard borrow projection used to lend a contained value.

The compiler does not hard-code `Unique`, `Shared`, reference counts, control blocks, or a particular
allocator. The standard library defines them as ordinary parameterized value types implementing the
lifecycle and ownership concepts, conceptually:

```text
concept Lifecycle {
    associated CopyEffects = effects
    associated DropEffects = effects
    operation drop = take Self -> unit ! DropEffects
}

concept Owner<T> : Lifecycle {
    # Borrowed result is tied to this receiver and cannot independently escape.
    operation borrow = &Self -> scoped &T
}

concept ShareableOwner<T> : Owner<T> {
    operation retain = &Self -> Self
}

concept StableAddressOwner<T> : Owner<T> {
    # The pointee address remains fixed for this owner's live storage generation.
    operation stable-borrow = &Self -> scoped stable &T
}

concept PinnableOwner<T> : Owner<T> {
    associated Pinned : StableAddressOwner<T>
    # Guaranteed nothrow and non-suspending: no allocation or reservation may newly fail.
    operation pin = take Self -> Pinned
}

concept TryPinnableOwner<T> : Owner<T> {
    associated Pinned : StableAddressOwner<T>
    # Failure returns the still-owned original carrier with the diagnostic.
    operation try-pin = take Self -> result<Pinned, PinFailure<Self>>
}

Unique<T> : Owner<T>                 # movable, not copyable
Shared<T> : ShareableOwner<T>        # copying retains a strong handle
Weak<T>                              # upgrade returns option<Shared<T>>
```

These mappings use the same explicit concept-evidence mechanism as ranges and other generic code;
they are not name-based duck typing. User-defined arenas, pools, foreign handles, and ownership
carriers may implement the same public contracts. A carrier's trusted implementation may use raw
allocation primitives, but ordinary code sees its checked lifecycle behavior.

Every `Owner<T>` keeps the pointee valid and at one address for the duration of each active borrow.
An ordinary owner may relocate its pointee only between borrows, when no derived address or view
survives. `StableAddressOwner<T>` strengthens that promise across the owner's whole live storage
generation. `PinnableOwner<T>` is the narrower guarantee: it consumes a relocatable owner and
returns a stable owner without a fallible allocation, reservation, exception, or suspension.
`TryPinnableOwner<T>` is used when promotion may relocate, allocate, reserve scarce pinned storage,
or otherwise fail. Its failure value contains the original live owner so a failed attempt never
silently destroys or strands ownership. Both operations require that no borrow or derived address
is active while pinning occurs, and any effects of fallible promotion remain explicit in the
callable contract.

Moving the owner handle need not move the pointee. Self-referential values, retained native
callbacks, asynchronous FFI buffers, and APIs that store an address require stable-address evidence;
ordinary borrowing does not acquire it accidentally. A raw address derived from a borrow remains
bounded by that borrow even when the pointee is stable, and pinning never grants authority or
extends ownership by itself.

Copy and drop effects are part of generic evidence and every callable's inferred effect row,
including implicit cleanup edges. They may perform bounded deterministic lifecycle work but cannot
suspend, allow an exception to escape, acquire ambient authority, or hide an externally fallible mutation. Resource
release authority travels with the owner that acquired the resource. General I/O, commit, flush,
and protocol shutdown belong in explicit `close`/`finish` operations or scope guards. Dynamic owner
evidence binds a fixed compatible cleanup effect row; erasure cannot conceal it.

Drop classifications are derived compiler evidence, not annotations that ordinary programmers must
sprinkle through records or call sites. The compiler derives the strongest class justified by every
field and custom drop hook:

```text
trivial-drop    no cleanup action; it may be erased completely
local-drop      deterministic memory-local release such as freeing or decrementing an owner
ordered-drop    bounded local cleanup with observable sequencing, such as unlocking a guard
```

They form the ordered lattice `trivial-drop < local-drop < ordered-drop`. The drop class of a record,
variant, closure environment, collection, or owner carrier is the join of every possibly live
field's class, the carrier's own bookkeeping, and its custom hook. Unused or provably uninhabited
storage contributes nothing; branch-dependent finality does not lower the conservative public
class. Generic code carries this derived class in lifecycle evidence rather than assuming a class
from the carrier's name.

In particular, `drop-class(Shared<T>)` joins memory-local reference-count release with
`drop-class(T)`, because the final strong release may destroy `T`; a non-final decrement being
cheaper does not change the callable contract. `Weak<T>` similarly includes its control-block
release but not `T`'s destructor, since a weak release cannot be the last strong owner. Fixed arrays
and homogeneous containers join the element class with their storage owner's class. This
composition rule makes nested cleanup and optimizer barriers predictable without pessimistically
classifying all reference counting as ordered.

All three preserve the language's exactly-once, reverse-construction destruction rules. The class
only tells optimization and generic code what is unobservable: a trivial drop may disappear, and a
local drop may be inlined or coalesced where ownership dependencies prove that timing and order are
unchanged. An ordered drop remains a sequencing barrier. Fallible, suspending, externally visible,
or authority-acquiring work is not a fourth kind of destructor; it belongs in `close`, `finish`, or
a scope guard. The compiler rejects a custom hook that cannot be certified into one of these bounded
classes and reports which operation or field prevented certification.

An ownership-taking API should not need to name `Shared<T>` merely because one caller uses shared
storage. If the callee only needs to hold and eventually release one owner, it accepts an ownership
carrier. If it must manufacture additional owners, its signature honestly requires
`ShareableOwner<T>`. Carrier dispatch is explicit just like behavioral concept dispatch:

```text
store(take x: static Owner<Foo>)       # carrier remains statically known/specializable
store-runtime(take x: dyn Owner<Foo>)  # erased owner for factories/open runtime sets
duplicate(take x: ShareableOwner<Foo>) # operation genuinely needs another owner
```

The static form retains checked parametric HIR and may specialize for `Unique<Foo>`, `Shared<Foo>`,
or another carrier. The dynamic form is one versioned ownership envelope containing sufficient
evidence to borrow and destroy the held value. It is appropriate for heterogeneous containers and
runtime factories. The compiler must not eagerly generate all ownership/behavior dispatch
combinations, infer dispatch from an expected result, or make separate overloads that the caller
cannot distinguish. A baseline evidence-table ABI permits one checked generic body; selective
specialization is an optimization.

### Stack, heap, and deterministic destruction

A plain lexical value is stack/frame-owned unless an explicit storage operation moves it elsewhere;
an optimizer may change physical placement only when that is unobservable. Safe heap allocation
always names an ownership policy. The `new unique`, `new shared`, and `share` spellings below lower
to standard-library carrier/allocator constructors; they do not give those types compiler-owned
layouts:

```lisp
(let local (Foo ...))
(let unique-foo (new unique Foo ...))
(let shared-foo (new shared Foo ...))
(let promoted (share local)) ; allocates shared storage and moves local
```

There is no safe unqualified owning heap pointer. Constructing `Shared<T>` from `&local` or any
other stack borrow is a compile error; promotion consumes the stack value and invalidates its old
binding. A raw pointer is a non-owning unsafe/FFI primitive and never acquires cleanup behavior by
accident.

Core safe memory management requires no tracing garbage collector. Frame ownership, moves,
explicit unique/shared carriers, deterministic drop, and bounded borrow analysis provide the
default storage model; reference counting is paid only by a chosen shared carrier. A host or
library may expose a tracing arena as an explicit owner implementation with declared safepoint,
pinning, finalization, and checkpoint behavior, but that extension cannot change ordinary record,
closure, or pointer semantics or make finalization nondeterministic for other owners.

All constructed values may define deterministic destruction. A stack/frame value is dropped when
its owning scope unwinds. A unique heap pointee is destroyed and deallocated when its `Unique`
owner drops. A shared heap pointee is destroyed and deallocated when the final strong `Shared`
owner drops; `Weak` handles do not keep the pointee alive. Consequently safe code cannot create a
heap object whose destructor is silently unreachable merely because no ownership policy was
attached to its allocation. Shared cycles can retain memory and must be broken with weak edges or a
different ownership policy; they are a diagnosable leak risk, not memory unsafety.

Reference counting does not collect a strong cycle as a group. If `A` strongly owns `B` and `B`
strongly owns `A`, dropping every outside `Shared` handle leaves both internal strong counts nonzero,
so neither destructor begins. At least one back edge must be `Weak`, an explicit operation must break
the cycle, or the graph must use an ownership policy such as an explicit tracing arena. This rare
case does not justify tracing overhead for every shared value. The compiler should warn for obvious
unconditional structural cycles, and debug runtimes may report retained strongly connected owner
graphs, but neither diagnostic may pretend to prove arbitrary runtime graphs cycle-free.

Drop is for deterministic, non-suspending cleanup. Fallible or asynchronous shutdown belongs in an
explicit operation such as `close`, `finish`, or `join`; drop may provide a safe fallback but cannot
hide its failure. Cancellation and typed exception unwinding run owned drops exactly once. A hard
process abort is not required to run user code.

### Safe-code boundary, subtyping, and variance

Ordinary verified CoLisp and Co-Forth have no undefined behavior. A definitely invalid operation is
a compile error. When a dynamic fact cannot be proven cheaply, checked code emits a defined runtime
trap carrying its source origin. Operations capable of violating the model—raw-pointer arithmetic,
unchecked access, manual allocation, and unverifiable FFI contracts—require an explicit unsafe
boundary. Every such construct is compiler-identifiable, warnable, searchable, and optionally
forbidden by project policy; a warning alone never turns an unsafe operation into safe code.

The safe surface may carry an opaque typed foreign address, compare it with null, and pass it back
through a declared safe wrapper, but it cannot inspect the address, perform arithmetic, dereference
it, convert it to or from an integer, manufacture a borrow from it, persist it, checkpoint it, or
send it to another worker. Those operations require a lexically explicit `unsafe` block inside a
callable whose contract states the caller-visible preconditions. Marking a callable unsafe does not
disable ordinary type, ownership, effect, initialization, or control-flow checks, and a definitely
invalid operation remains an error. Unsafe instructions and their source origins survive lowering
and module certification; they are never erased into an apparently safe call edge.

Hosted Finch admits no unsafe source or IR. The interactive client, daemon, provider/model execution,
remote execution, and other hosted profiles reject a module containing unsafe instructions or an
unsafe call edge—even when unreachable—and provide no prompt, grant, or capability that can override
that decision. A `ModuleVerified` artifact therefore carries an explicit unsafe summary, and hosted
admission requires it to be empty. Unsafe execution exists only in an eventual explicitly unhosted
native/embedder profile whose build and policy both opt into the unsafe runtime surface. Even there,
unsafe code does not gain filesystem, process, network, or other host authority; those remain
separate typed capability effects. Trusted native host plugins are outside the hosted language
sandbox and remain part of the host's auditable TCB rather than a way for hosted Finch code to enter
unsafe mode.

Subtyping follows capability and mutation rather than class layout. Function inputs are
contravariant and results covariant. Readonly borrows and readonly owner views may be covariant;
mutable borrows and mutable ownership containers are invariant. A dynamic implementation may upcast
to a concept whose requirements are a subset, immutable records may support explicit width
subtyping, and immutable closed variants such as the library `result<T,E>` are covariant in their
payload types. A handler accepting a broader exception value can handle a narrower thrown value;
handler input is contravariant. A callable with fewer effects, no escaping exception edges, or a
verified `nothrow` guarantee is usable where a less restrictive callable is accepted. Core CoLisp
and Co-Forth have no class inheritance, implicit implementation inheritance, or storage-layout
diamond. Composition, explicit delegation, variants, and concept evidence provide reuse and
polymorphism.

`Shared<T>` lends readonly access by default. Possessing one handle can never prove alias-wide
exclusive access, so it does not directly provide a mutable borrow of `T`. Mutation requires a
library type whose evidence enforces the rule—an atomic, mutex, actor, transactional cell, or an
operation that proves sole ownership and recovers a unique carrier. Moving a unique owner between
workers requires `Transfer<T>` evidence. Retaining a shared owner across workers additionally
requires `ShareAcrossWorkers<T>` evidence, normally derived only for immutable values or explicitly
synchronized containers. Reference counting alone is not a thread-safety claim.

**Shared ownership never implies shared mutability.** Safe code, concept adapters, and optimizers may
assume that aliases obtained through `Shared<T>` are readonly unless separate synchronization or
recovered-uniqueness evidence explicitly permits mutation.

### Receiver mutability, deep immutability, and copy-on-write

Receiver access is the ordinary mutability contract. A receiver defaults to readonly `&self`;
mutation requires `&mut self`, and ownership escape or destruction requires `take self`. An
operation declared with `&self` is usable through mutable, readonly, unique, shared, or deeply
immutable storage because it promises not to mutate through that receiver. The compiler rejects
mutation in such an operation and should diagnose an unnecessarily exclusive private receiver, so
one implementation does not need mutable/const/immutable/inout overloads merely to express that it
does not write.

Readonly access is not a D-style transitive type constructor applied implicitly to the entire
reachable object graph. It prevents mutation through that borrow. Deep immutability is a separate
explicit guarantee, represented by immutable/frozen owner evidence, and is required only by APIs
that depend on permanent non-mutation, cross-worker sharing without synchronization, ROM placement,
or similar properties. Interior mutation remains possible only through a carrier whose public
evidence states the synchronization or transactional policy.

Copy-on-write is likewise an explicit library ownership policy such as `Cow<T>`, not the default
semantics of record member calls or `Shared<T>`. Read projection from `Cow<T>` never clones; an
exclusive edit may prove uniqueness or clone using declared copy/clone evidence before lending
`&mut T`. This makes the possible allocation visible in the carrier type and excludes resources,
pinned/self-referential values, identity-sensitive objects, and other values without valid clone
evidence. The optimizer may remove a uniqueness check or allocation when it proves the carrier is
already sole and the change is unobservable.

### Borrowed results, ranges, closures, and suspension

A borrowed view, range, iterator, or slice may remain allocation-free inside the scope that owns its
source. It cannot escape that ownership boundary without becoming owner-carrying. A returned or
stored range therefore owns its source carrier, retains a shared source, materializes its contents,
or uses an explicit dynamic owner envelope. This permits Voldemort adaptor types and fused
stack-local pipelines without exporting Rust-style lifetime parameters.

A closure with inferred or explicit borrowed captures is valid only while the compiler proves that
it remains within the invocation and never suspends. An escaping, stored, returned, deferred, or
dynamically erased closure must use `:move` or an explicit capture list that supplies owners: unique
captures move and shared captures retain according to their carrier behavior. The same rule applies
to async state machines and continuations. No borrowed reference survives a checkpoint, worker
migration, or host-effect suspension, and escape analysis never silently promotes a borrow into an
owning capture.

These restrictions intentionally trade a small amount of Rust's most general borrowed-return
expressiveness for bounded local dataflow analysis, predictable compilation time, and errors at the
operation that moved or attempted to escape a value. They preserve static types, native layout, and
specializable calls; convenience never falls back to JavaScript/Python-style runtime duck typing.

### Transactions, checkpoints, and persistent values

Copyability, shareability, and checkpointability are independent concepts. `Shared<T>` is not
serializable merely because its process-local handle can be retained, and a live `Unique<T>` cannot
be duplicated to preserve both a working revision and a rollback snapshot. Values crossing a
persistent VM revision, checkpoint, replay, process, or durable task boundary must provide explicit
`Checkpointable` evidence defining a versioned value encoding and reconstruction semantics.

The initial durable stack accepts immutable serializable values and generation-checked host resource
handles, not arbitrary heap pointers or live unique resources. A non-checkpointable unique value is
execution-local and must be consumed, explicitly closed, converted to a checkpointable value/host
handle, or dropped before suspension or commit. A shared carrier crosses only when both the carrier
and pointee contract define checkpoint behavior; replay reconstructs a new owner and never promises
the same address or reference count.

Transactions retain an immutable committed revision and build a separate owned delta. Commit moves
checkpointable results into the new revision. Rollback drops newly created execution-local owners
and restores the unchanged committed values; it does not resurrect a resource that user code has
already destroyed or claim that an external effect was undone. Retains created specifically for a
durable revision are explicit cleanup obligations of that revision and release when the revision is
retired. This keeps final-drop timing deterministic relative to revision ownership rather than an
invisible snapshot copy.

### Co-Forth parity and IR verification

Every ownership construct exposed by CoLisp must have a direct typed Co-Forth spelling or word:
borrowing, taking, unique/shared/weak construction, promotion, static/dynamic owner evidence, drop,
unsafe boundaries, variant construction/destructuring, `throw`, handler regions, catch patterns,
`nothrow` guarantees, record layout declarations, safe receiver projection, and inferred/move/exact
closure capture policies. Co-Forth stack effects record whether an input is borrowed or consumed,
and its handler and closure syntax lower to the same exceptional edges, match decision trees,
capture records, and ownership transitions, so its direct operation mapping to typed IR loses no
source-level guarantee. CoLisp lowers the same semantics rather than routing through Co-Forth text.

The common IR records moves, owner/evidence erasure, borrows where relevant to verification, and
cleanup edges. Its verifier rejects use-after-move, double drop, leaked required ownership, escaping
borrows, mutable aliasing, and borrows live across suspension. The interpreter and future Cranelift
backend consume those already-verified decisions; native lowering never reruns source inference or
invents a different lifetime model. A shared conformance corpus must express each ownership behavior
in both syntaxes and compare accepted IR, diagnostics, traps, drops, and observable results.

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

Initially exclude general continuations. There is no user-facing `eval` that runs an arbitrary
tree in the current environment. Mix-back of generated syntax into a **module** is compile-time
only (CTFE, ordinary `if` when the condition is a compile-time constant, generics, `splice`).
Shipping a program to a node is compiling a **compilation unit** with a granted capability set.
The escape hatch below (`interned` compiled callables) is not `eval`: it returns a **function**,
does not interpret trees per call, and still runs expand/check/verify. Add continuations only
with a clear typed/effect model.

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
instantiations whenever the program determines them. Private, non-recursive pure definitions may
generalize their locally inferred type variables. Apply an effect-aware value restriction: a
definition that allocates mutable state, captures a capability, suspends, or otherwise has an
observable effect remains monomorphic unless its type parameters are explicit. Recursive and
public definitions, effect and capability-selector boundaries, refinements, and FFI require
declared signatures; publication validates and freezes the declaration rather than exporting an
accidentally inferred contract. Exception flow remains inferred throughout the body, but a published
definition chooses `nothrow`, an explicit exception upper bound, or `throws infer` to make its API
stability policy visible. It likewise chooses `non-suspending`, a `suspends` upper bound, or
`suspends infer`; private/intermediary definitions infer both clauses. Concepts, parameter
packs, ranges, overload resolution, and bounded CTFE should
make routine code feel as direct as Python or JavaScript while retaining a static, optimizable
execution path. Do not achieve convenience by silently inserting `dynamic`, unchecked coercions, or
an interpreter-only fallback.

Rust's inference is Hindley-Milner-derived but extends it with traits, regions, coercions, and other
constraints. CoLisp deliberately chooses a smaller boundary: inference proceeds forward between
bindings, while expected types flow inward only while checking the current expression. An
initializer or literal therefore establishes a local binding's type; subsequent calls check that
known type against their parameter contracts. Expression-local expectations may type an empty
collection constructor, lambda parameter, or branch when all choices are within that expression.
They do not select an overload or dispatch mode from its expected result and may not use a later
statement to revise an earlier binding. For example,
`let foo = 3; bar(foo)` with `bar : string -> ...` diagnoses the argument at `bar(foo)`, not `3`,
and does not solve `foo` backward as `string`. Generic type/value arguments likewise flow from
explicit arguments and supplied values into a bounded instantiation. This keeps inference
incremental and gives both humans and models a stable primary blame location.

### Lowering

The frontend performs:

1. parse with exact source spans;
2. hygienic expansion by ordinary bounded CTFE (`syntax -> syntax`), not a second evaluator;
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

Finch has no string mixin or `compile(text)` facility. Compile-time code cannot manufacture source
bytes and ask a frontend to parse them inside the current module; that would create a second parse
boundary, discard hygiene and binding identity, and move diagnostics onto generated text. The useful
declaration-composition behavior sometimes called a mixin is expressed by a structured syntax macro
that returns declaration nodes. It may generate fields, callables, nested declarations, attributes,
or explicit concept evidence, all of which retain expansion provenance and pass through the normal
coherence and verification pipeline. Optional `mixin` surface sugar may only invoke that ordinary
structured macro protocol; it is not inheritance, textual member injection, or another expansion
engine. Runtime composition remains record embedding, delegation, and concept evidence.

Quote produces `syntax`, not a stripped runtime list. `'form` and nested `quote` mean the form is
**data**: it is not evaluated at that site. Quasiquote `` ` `` builds syntax by template; unquote
`,` fills a hole with a `syntax` or CTFE value; splice inserts a list of syntax values into a
surrounding list. These operations copy origin and expansion ancestry onto constructed cells.
They never parse source bytes. The current symbol-only `quote` restriction is transitional and
must be replaced by this full nested quote/quasiquote/unquote/splice over `syntax`.

A CTFE function on numbers is not a macro: `(+ 1 2)` at compile time is the integer `3`. A CTFE
function whose contract is `syntax -> syntax` **is** a macro. The only remaining difference from
an ordinary function is **call convention**, not a second evaluator:

- function: arguments are evaluated first; the caller writes `(expand-when '(when ready? (pkg.ensure nginx)))`;
- `define-syntax`: arguments are not evaluated; `(when ready? (pkg.ensure nginx))` is compiled as
  `(splice (expand-when '(when ready? (pkg.ensure nginx))))`.

`splice` is an ordinary **compile-time** word (Co-Forth: the explicit splice word already listed
with `macro:` / `syntax[ ... ]`). Its meaning is: this `syntax` **value** is the next form in the
**module being compiled**. It is not runtime `eval`. The compiler’s following phases (check, lower,
verify) treat that tree as source. The expander **returns data**; later phases make it executable.
Calling `pkg.ensure` inside a `syntax -> syntax` function would be a compile-time host effect and is
forbidden; list surgery and quasiquote only **construct** forms.

Staging follows D’s CTFE/generics more than Lisp-with-eval, without D’s extra `static if`
keyword. **CTFE** on values: if an `if` condition is a compile-time constant, that `if` **is**
compile-time — the dead arm is dropped and is not type-checked as residual IR (the live arm still
is). There is no `static-if` form. **Generics** instantiate then type-check (the instantiated IR is
what the verifier sees). **Syntax CTFE** only where evaluation order or bindings cannot be a
function. Every generated form is checked with the same rules as handwritten code. Diagnostics
name user form, pretty-printed expansion, and fault span (SDC mixin style). There is no untyped
`defmacro` and no user `eval`.

`let` in such a function is the usual expression: bindings, then a body whose **value** is the
result (typically a quasiquoted list). Nothing further is bound unless the caller `define`s a
name or `splice`s the value into the module.

A transformer that only wraps two calls is not a reason to use syntax CTFE. That is an ordinary
function:

```
(define (require-pkg name)
  (pkg.ensure name)
  (svc.enable name))

(require-pkg "nginx")
```

Syntax CTFE is for forms a function cannot implement because it would **evaluate** arguments.
`when` must not run `pkg.ensure` unless `ready?` is true:

```
(define (expand-when form)
  (let ((test (second form))
        (body (third form)))
    `(if ,test ,body #f)))
```

`expand-when` only **builds a list**. It does not run `if` or `pkg.ensure`. `splice` pastes that
list **once** into the module as source — it does not define a word named `when`:

```
(splice (expand-when '(when ready? (pkg.ensure nginx))))
;; the next form in the file is:
;; (if ready? (pkg.ensure nginx) #f)
```

`define-syntax` **registers** the transformer so every later `(when …)` is implicit-quote plus
splice. Without it, `when` is not a callable; you would write `splice` at each use. Type and
effect checking apply to the **expanded** `if` tree. A converge `require` that yields until
another form is Done is the same shape: syntax, not “two host calls with a wrapper name.”

Diagnostics for syntax CTFE follow SDC mixin reporting, not DMD’s “blame the mixin line.” A
failure names (1) the **user form** and its span, (2) a pretty-printed **expansion** the
transformer actually returned, (3) the **fault** in that expansion with a span, plus ancestry
to the transforming function. Dropping spans on quote/quasiquote is a defect: CTFE over
spanless lists is a string mixin. Fuel, recursion, and allocation limits still apply.

A Brain or node that **loads** a CoLisp payload compiles it as a compilation unit (expand, check,
lower, verify) with a granted capability set. That is the compiler invoked by the host, not a
language `eval`. Generating syntax as data is always allowed; executing it is never ambient.

**Interned compiled callables (LINQ-style compile cache).** When a mapping or binder is not known
until runtime (DB row → record, query shape × type), user code may ask the **compiler as a
library** for a function:

- Input: `syntax` and/or a `type` (and a stable fingerprint of the query/schema), not a string of
  source.
- The compiler runs the same expand → check → lower → verify pipeline and returns a **typed
  callable** (bytecode or JIT).
- The host **interns** that callable under `(fingerprint, type-id, capability set)`. The first
  use compiles; later uses are an ordinary call. Per-row work must not re-expand or re-verify.
- Diagnostics are SDC-style on that compilation (user form / expansion / fault), not a runtime
  interpreter stack.
- This is C# `Expression.Compile` plus a cache, not Lisp `eval`. Prefer CTFE/generics when the
  record type is known at module compile (zero runtime compile). Use the interned hatch when the
  shape is only known then (ad-hoc query, reflected schema).
- Capability and fuel apply to **compilation** as well as to the resulting function’s effects.
  An LLM does not get this hatch unless that Brain is granted it.

Until that kernel exists, `define-syntax` remains a capture-free **template**: substitution
before type checking, no CTFE body, no capabilities, and no introducing `let` or other binding
forms. That path is deleted after migration fixtures prove `syntax -> syntax` CTFE plus `splice`
preserve hygiene, spans, and IR.

The S-expression is the visible structural notation, while `Syntax` is the compiler-facing value.
An identifier syntax object carries its spelling, scope marks, phase, source origin, and eventually
its resolved binding identity; destructuring or taking the head/tail of syntax must not discard that
metadata. `syntax->datum` is an explicit lossy conversion to ordinary runtime symbols/lists.
`datum->syntax` must receive a lexical context explicitly, and constructing a hygienically fresh
identifier is distinct from deliberately requesting a caller-context identifier. Implementations
may store syntax in arenas with compact metadata IDs; this logical contract does not require a fat
allocation for every atom.

Every source-visible core form has public hygienic constructors and projections even when its
lowering is compiler-defined. In particular, lambda syntax exposes an introspectable `CaptureSpec`
with a default policy and ordered `CaptureEntry` values, parameters, body, and origins. Before name
resolution a capture entry contains syntax identifiers; afterward it records stable binding IDs,
types, ownership modes, and source origins; closure conversion records final field indices and
capture operations. Syntax macros run before capture analysis, so free bindings introduced by an
expansion participate in the completed capture set. Typed later-stage reflection may inspect the
resolved plan but cannot mutate compiler-private physical offsets. Co-Forth syntax construction
exposes the same semantic capture nodes rather than requiring generation of CoLisp text.

An expansion may emit ordinary type, callable, and concept-implementation declarations. This is
how a derive macro can generate serialization code *and* publish the explicit evidence that the
record satisfies `JsonSerializable`; generating methods with familiar names is never sufficient.
Generated implementations enter the same post-expansion name-resolution, coherence, visibility,
effect, and verification passes as handwritten ones. Hygiene gives generated helper callables stable
private identities, so two derives may both implement an operation spelled `serialize` without
creating a global-name collision. Diagnostics retain both the derive invocation and generated
implementation origins.
Each generated implementation/evidence binding also receives a stable module-qualified identity.
Two expansions that publish the same evidence identity are a duplicate-definition error; differently
named implementations for the same concept and concrete type remain distinct and make unqualified
ambient resolution ambiguous. Expansion or import order never replaces or selects evidence.

Classic S-expressions remain one exact, canonical structural reader, not a requirement that every
human-facing Lisp spelling pay the full parenthesis cost. Later expression/indentation/call sugar
may provide forms such as `foo(a, b + c)`, but the reader must convert each convenience spelling
immediately into the same syntax tree before expansion or semantic analysis. Sugar never adds a
second semantic construct, staging rule, or compiler lowering path. The property to preserve is
syntax-as-ordinary-data, not a mandate that every surface syntax look homoiconic. Conformance tests
must pair every convenience spelling with its canonical S-expression and prove structural syntax
equivalence after ignoring spelling-specific source origins, followed by identical elaborated
HIR/IR. This is reader notation, not a third `FinchScript` language or frontend.

## Generics, concepts, dispatch, and metaprogramming

Lisp and Co-Forth expose the same facility and equivalent uses lower to equivalent typed IR.
Concept satisfaction, generic code generation, and call dispatch are separate decisions. A concept
states required operations and associated types, values, and effects. An implementation explicitly
maps each requirement to a stable member or free-function identity rather than relying on matching
source spellings:

```text
implementation MyListRange<T> : Range {
    associated Item = T
    associated Effects = {}
    operation empty?    = my-list-empty?#41
    operation front     = my-list-front#73
    operation pop-front = my-list-pop-front#74
    dynamic-evidence-version = 1
}
```

An operation requirement defines one canonical receiver and call ABI. Receiver forms are
readonly `&self`, exclusive `&mut self`, consuming `take self`, or no receiver for an associated
operation. An implementation mapping is a type-checked receiver adapter, not only a function-name
alias: it binds the concept parameters and states exactly how they reach a member, namespaced/static
function, free function, generated callable, or composed delegate. For example:

```text
concept JsonSerializable {
    associated Output = bytes
    operation serialize(&self, options: &JsonOptions) -> Output
}

implementation UserJson for User : JsonSerializable {
    operation serialize(&self, options) =>
        UserCodec.serialize(options, self)
}

implementation WidgetDrawable for Widget : Drawable {
    operation draw(&self, canvas) =>
        Drawable.draw(&self.presentation, canvas) using PresentationDrawable
}
```

The canonical Co-Forth declaration shape carries the same fields rather than relying on matching
word names:

```forth
concept: JsonSerializable
  associated: Output type ;
  operation: serialize ( S borrow Self borrow JsonOptions -- S Self JsonOptions Output ) ;
;

implementation: UserJson for User : JsonSerializable
  associated: Output = bytes ;
  operation: serialize { self options -- }
    options self UserCodec.serialize
  ;
  dynamic-evidence-version: 1 ;
;

user options JsonSerializable.serialize using UserJson
```

The implementation operation body is a checked receiver adapter with named inputs; `using` is
compile-time evidence selection and does not consume a runtime stack value.

The adapter may explicitly reorder arguments or project a composed receiver. A direct
`operation serialize = encode-user-json` shorthand is valid only when the callable already has the
exact canonical signature, receiver ownership, argument order, result, effects, and exception
contract. Static use can inline the adapter; a dynamic evidence slot points at a canonical-ABI
adapter thunk. The verifier rejects an adapter that takes through `&self`, obtains mutation without
exclusive access, lets a receiver-tied borrow escape, widens the operation's effects/exceptions, or
performs an implicit representation conversion.

These spellings are illustrative until the surface grammar is frozen. The declaration is evidence,
not inherited implementation or an implicit method search. It may publish only static evidence, or
additionally publish a versioned dynamic evidence table when the concept has a fixed runtime ABI. A
derive tool may generate the declaration, but the compiler still consumes an explicit mapping
rather than silently treating matching names as conformance. Exported evidence is named and stable.
Each requirement has exactly one selected mapping in a compilation context; competing equally valid
evidence is an ambiguity error, never an import-order decision.

Concept evidence is never made ambient merely by loading or importing its defining module. Every
implementation has a stable qualified name. A call either names it with `using`, receives it through
a generic evidence parameter, or uses one default explicitly imported into that lexical compilation
context. Initially only the module that defines the concept or the concrete type may publish such a
default; third-party implementations remain named. A module records every selected evidence identity
in its sealed interface, so adding another import cannot retrospectively change dispatch or make an
already compiled call ambiguous.

Operation names live in their concept evidence rather than one shared method namespace. For
example, independent derives may map both `JsonSerializable.serialize` and
`BinarySerializable.serialize` for the same record to different hygienic callables. Static code
selects the intended evidence explicitly with a concept-qualified operation or a named evidence
argument; it does not need to erase or cast the value merely to disambiguate a name:

```text
JsonSerializable.serialize(user) using UserJson
BinarySerializable.serialize(user) using UserBinary
```

Erasing `user` as `dyn JsonSerializable using UserJson` is the corresponding deliberate runtime-
dispatch choice: the erasure site names the evidence, policy, or wrapper type, and the resulting
existential carries `UserJson`'s evidence table, so its `serialize` slot is
unambiguous. A runtime checked `as-concept` lookup serves an already erased/dynamic value whose
concrete evidence is not statically known; it is not ordinary static overload resolution. If a type
has several implementations of the *same* concept (for example canonical and compact JSON), none is
ambiently preferred: the caller must supply a named implementation, policy value, or wrapper type,
including at a dynamic-erasure site. The expected result type, generated helper spelling, and import
order never choose between them.

Dispatch mode is explicit in each template or function type contract, independently for every
argument. `R : Range<Item=T>` has one defined mode—static evidence; `static Range<Item=T>` (or
concise anonymous-static sugar) spells the same intent directly. `dyn Range<Item=T>` is a distinct
runtime existential type, while `some Range<Item=T>` is an opaque return whose concrete type is
hidden but statically fixed. A mixed function may accept one static and one dynamic concept
argument without generating every static/dynamic combination. The compiler never implicitly cracks
a `dyn` value to satisfy a static parameter or silently erases a concrete/static value, so one
argument cannot match both forms; static-to-dynamic erasure is an explicit conversion. A generic
caller propagates each dispatch mode in its own signature or performs that erasure at its boundary.
There is no ambient specialization choice.

A generic body is type-checked once, before instantiation, against its declared concepts and
associated outputs. Its retained parametric HIR records the required evidence operations. A call
then infers forward from explicit and ordinary arguments, resolves immutable cacheable evidence,
binds associated types/values/effects, validates remaining arguments, and reuses the checked body.
Candidate selection must not compile arbitrary bodies to see which one succeeds. D-style
`static if`, `is(...)`, and `__traits(compiles)` probes, C++-style SFINAE, and unconstrained
"does this expression compile?" reflection are not the concept-resolution model.

An exported generic publishes a versioned verified parametric artifact, not compiler-private HIR.
That artifact contains the generic's semantic operations, type/value/pack parameters, constraints,
ownership and effect transformations, source origins, and explicit evidence-table parameters in a
canonical representation accepted by the independent verifier. Downstream compilers instantiate or
share that artifact without reparsing source or depending on an internal AST layout. A concrete
instantiation is an ordinary fixed-signature function; specialization is a rebuildable cache keyed
by the parametric artifact and selected arguments/evidence.

Generic parameters marked `infer` are outputs of evidence resolution rather than variables in a
global back-solving system. For example, `T : Map<K,V>, infer K, infer V` derives `K` and `V` from
the selected `Map` implementation, while callable evidence may similarly derive an argument pack,
result, and effect row.

Static evidence does not require unconditional Rust-style monomorphization. The baseline generic
body uses one uniform evidence/dictionary ABI: a static argument supplies a constant evidence table
and a dynamic argument supplies its runtime table. This avoids eagerly producing up to `2^n`
static/dynamic variants for mixed arguments. The compiler selectively specializes static,
layout-dependent, or hot combinations when there is a proven benefit; direct calls and constant
evidence may still be inlined. Cache keys include the generic definition, type/value arguments,
evidence identities, effects, target, and ABI so identical uses do not repeat semantic work or
native compilation.

A dynamic concept packages an existential value or generation-checked resource handle with a
versioned evidence table. Associated types needed by callers are bound, and every exposed operation
has a dyn-compatible fixed ABI; generic methods, unbound `Self`, or compile-time-layout-dependent
operations remain static unless explicitly reified through that ABI. Dynamic dispatch is the right
tool for heterogeneous collections, plugins, and especially runtime factories whose concrete
result depends on configuration or runtime input. Runtime acquisition such as
`as-concept(value, Reader<Item=bytes>)` performs an identity-based registry lookup and returns
`option` or `result`; it never scans method names or treats a failed call as conformance.

Dynamic evidence belongs to the erased view, not the concrete record layout. A borrowed view is
conceptually `(data pointer, selected evidence-table pointer)`; an owned existential additionally
retains the actual owner/lifecycle evidence required to destroy its storage. The evidence table is
immutable shared module data, not copied into each object. Consequently a concrete record contains
no mandatory vptr, may remain inline, and may simultaneously form different concept views or use
different named implementations without mutation. An implementation declared in another module
emits its own stable evidence table; forming `dyn C using Implementation` selects that table and
does not rewrite existing values. Conversion from an already erased value uses its runtime type
identity and the sealed module's versioned evidence registry, returning `option`/`result`; loading
new evidence may extend a new verified composition epoch but never retrofits an existing view in
place.

### Explicit dynamic property and invocation hooks

Ordinary member failure remains a compile error unless the receiver explicitly supplies dynamic
property evidence. The standard library may define a facility conceptually like:

```text
concept MissingProperty<V> {
    operation property-get(&self, name: symbol) -> option<&V>
    operation property-get-mut(&mut self, name: symbol) -> option<&mut V>
    operation property-set(&mut self, name: symbol, take value: V) -> option<V>
}
```

`JSONValue`, a string-keyed map wrapper, row, proxy, or similar type may opt in with a named
implementation. Resolution tries real fields/members first and then the uniquely selected
`MissingProperty` evidence; arbitrary keys remain available through indexing. A constant member
name may carry a precomputed hash or receive a JIT shape/offset fast path, but absence still returns
the declared `option`/`result`. This hook never establishes concept satisfaction, rescues failed
overload resolution, suppresses errors in its implementation, or manufactures JavaScript-style
`undefined`.

Dynamic method invocation is a separate `DynamicInvoke` concept with a fixed argument/result and
effect/exception contract. Accessing `json.customer` may therefore request a JSON property without
making `json.send-email()` silently execute a name-selected operation. Real-member shadowing makes
indexed access the permanent unambiguous spelling for colliding property names.

Keep three terms distinct. An **overload** is a compile-time choice among declared signatures; a
concept **implementation** is the explicit requirement-to-callable mapping that produces evidence;
an **override** is the concrete callable occupying a slot in dynamic evidence. Overload ranking is
finite and deterministic, using explicit generic/dispatch arguments and already-known argument
types only; import order and an expected return type never break ties. A concrete value cannot make
an exact `dyn` signature compete with a static generic because erasure is explicit. Overloads may
not differ only by result representation or dispatch. If identical inputs support a static adaptor
returning `some Range` and an erased adaptor or runtime factory returning `dyn Range`, require an
explicit dispatch argument/type application such as `(map :dispatch static ...)` versus
`(map :dispatch dyn ...)`, or give the operations distinct names.

Concept evidence conveys behavior, not authority. Possessing a table whose operation eventually
requests `fs.read<R>` neither creates a capability nor bypasses the broker; its declared effect and
the caller's actual grant are still independently verified at the host boundary.

Prefer composition in this order: records and functions, modules, closed variants for known
alternatives, explicit delegation, static concepts, and then dynamic concepts for intentionally
open runtime sets. Core CoLisp has no class or implementation inheritance. Object identity,
storage, code reuse, subtyping, construction, and dispatch remain independent facilities, avoiding
layout diamonds and brittle base-class contracts.

Compile-time reflection exposes immutable `type`, schema, syntax, symbol/module-reference, and
constraint-evidence values to pure bounded Finch functions. Generics, concepts, compile-time
branching/traversal, derive operations, and hygienic macros use this one staged evaluation model.
Generated definitions are structured syntax with expansion provenance and are verified normally;
string mixins and overlapping special-purpose metaprogramming subsystems are not part of the
design.

### Parameter packs, runtime rest arguments, and C varargs

Finch has no single overloaded notion of “variadic.” Declaration syntax selects one of three
facilities, and overload resolution never changes that choice from an expected result.

Canonical declaration shapes are:

```lisp
(define (render-all <(types Ts...)>
                    (args : (params (borrow Ts)...)))
  : string
  (ct-foreach ((T arg) args) ...))

(define (sum (values : (rest-borrow int))))
```

```forth
: render-all < types Ts... >
  ( S params<borrow Ts...> -- S string )
  ct:foreach-param ...
;

: sum ( S borrow rest<int> -- S rest<int> int ) ... ;
```

A compile-time heterogeneous parameter pack is kinded. `types Ts...` contains types; `values xs :
Ts...` contains corresponding compile-time values; and `params ps...` contains ordered parameter
descriptors carrying type, ownership mode, binding identity, and source origin. Packs may be empty
and are initially final in their parameter list. Bounded CTFE may inspect, slice, destructure, zip,
and `foreach` over them, but expansion occurs only at an explicit expansion site. Each argument is
evaluated exactly once from left to right and retains its own borrow/take/value/retain contract.
Instantiation erases the pack abstraction and produces an ordinary fixed-arity callable signature.
An uninstantiated pack-generic is not a first-class closure, callback, dynamic evidence slot, or FFI
function; it must first be selected and instantiated. A pack cannot be addressed, stored, returned,
or carried across suspension unless explicitly reified as a `tuple<T...>`, list, or other owner.

A runtime-variable homogeneous rest parameter is instead one fixed-ABI collection operand.
`rest-borrow<T>` receives a call-scoped readonly slice and cannot escape; an ordinary owned
`list<T>` (or another explicitly selected collection/range) may be taken and retained. Call syntax
may construct or explicitly spread a collection into that operand, but the declaration determines
the representation and no callee consumes an unknown number of ambient stack cells. Exact fixed
arity ranks ahead of a rest or pack candidate; a pack candidate participates only when its declared
constraints resolve without compiling arbitrary bodies. Arity and CTFE expansion have explicit
implementation limits with structured diagnostics.

CoLisp call parentheses already delimit candidate arguments, while Co-Forth requires a retained
structured boundary because a row-polymorphic stack cannot determine where `S` ends and `Ts...`
begins:

```text
CoLisp:   (render-all 1 "x" true)
Co-Forth: args{ 1 "x" true } render-all

CoLisp:   (sum :spread integers)
Co-Forth: integers rest-spread sum
```

`args{...}` is compile-time call syntax and lowers to fixed operands; it is not a runtime container
or marker. `rest{...}` may construct the declared homogeneous borrowed/owned rest operand. Supplying
an existing tuple, list, or range requires explicit `pack-spread` or `rest-spread`; ordinary values
are never flattened implicitly.

C ABI `...` is a third, unrelated facility available only to explicitly unhosted unsafe FFI. It is
part of the callable's linkage/type, requires at least one named parameter, follows the selected
target's default argument promotions, and accepts only supported C-ABI scalars, raw pointers, and
explicitly permitted `repr(C)` aggregates. Every variadic argument has an explicit promoted ABI type;
Finch strings, managed owners, closures, resources, borrows, exceptions, suspension, and ownership
transfer cannot cross `...`. Initially Finch may call imported C-variadic declarations through a
fixed-signature thunk per call-site type vector, but cannot define a C-variadic callee or callback.
There is no D-style Finch linkage with hidden raw argument pointers or runtime `TypeInfo[]`; dynamic
Finch values use ordinary checked types instead of creating another calling convention. This is an
intentional simplification of the three function-level forms in the
[D variadic-function specification](https://dlang.org/spec/function.html#variadic-functions), not an
assumption that D's native type-info variadics are C compatible.

Canonical unhosted spellings make every promoted argument visible:

```lisp
(extern "C" (printf (fmt : (c-ptr const c-char)) (args : c-varargs)) : c-int)
(unsafe
  (printf fmt
          (c-vararg c-int count)
          (c-vararg c-double ratio)))
```

```forth
extern(C): printf
  ( S value c-ptr<const-c-char> c-varargs -- S c-int )
;

unsafe[
  fmt
  cargs{ count c-vararg<c-int> ratio c-vararg<c-double> }
  printf
]
```

Here `cargs{...}` is retained unsafe call-site syntax from which the fixed thunk is generated, not a
portable runtime container or a `va_list` that safe code may inspect.

Constraint construction retains the source span that introduced it, macro invocation and
definition ancestry, generic definition, chosen evidence, and specialization or dynamic-erasure
site. A failure reports the nearest actionable expression as primary blame and the shortest
relevant chain as related spans. Together with span-bearing syntax, expansion ancestry, retained
typed HIR/IR, and stable diagnostic codes, this makes expanded Lisp debuggable without dumping raw
macro output or blaming a distant generic declaration.

## Common typed IR

Create a stable internal model resembling:

```text
Module
  version
  constants
  type table
  capability requirements
  imports by immutable ProgramRef
  verified parametric artifacts
  functions
    signature
      parameter ownership/receiver modes
      effect, exception, and suspension contracts
      linkage/calling convention/variadic and target ABI classification
    inferred_exception_set
    published_exception_contract = nothrow | upper_bound(types) | infer(types)
    locals/captures
    basic blocks
    instructions with SourceOrigin

Instruction examples
  Const, Copy, Move, Drop, Pick
  LocalGet, LocalSet, CaptureGet
  RecordNew, FieldGet, VariantNew
  BorrowShared, BorrowExclusive, OwnerErase, OwnerRetain
  Call, CallClosure, TailCall, Return, Throw
  Branch, CondBranch, Match, HandlerEnter, HandlerExit, Rethrow
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
- ownership-state agreement at control-flow joins;
- exactly-once destruction of owned values and absence of use after move;
- no escaping borrow, mutable alias, or borrow live across suspension;
- signature agreement on every return;
- the canonical inferred exception antichain is derived from every throw and throwing call edge and
  equals the function's stored `inferred_exception_set`;
- catch patterns are well ordered and narrow/bind only valid payload layouts;
- cleanup edges run before handler entry and an unmatched catch resumes the original unwind;
- the published exception contract is proved against that derived set: it is empty for `nothrow`,
  every inferred member is a subtype of some explicit upper-bound member, and `throws infer` stores
  exactly the same canonical antichain;
- transitive effects and capability selector containment;
- valid immutable dependency versions;
- bounded static limits where available;
- absence of forged handles or capabilities;
- well-formed suspension and resumption types.

Verifier output is a reusable certificate summary keyed to the exact module hash. Remote peers may
send summaries for caching, but each receiver verifies the module independently.

## Runtime, memory, and concurrency

### Concurrency memory model

Safe Finch is data-race-free. Ordinary mutable storage has one exclusive borrower and cannot be
observed concurrently; immutable values may be shared when their ownership evidence permits it.
Cross-worker transfer requires `Transfer<T>`, and retaining an owner across workers requires
`ShareAcrossWorkers<T>` plus immutable or explicitly synchronized interior state. A scheduler moving
a suspended task between workers does not relax those rules.

The memory model defines happens-before edges for task creation and ownership transfer, successful
join, mutex unlock/lock, channel or actor send/receive, transaction publication, and atomic
release/acquire or stronger operations. Standard atomics default to sequential consistency for
ordinary source; weaker `relaxed`, `acquire`, `release`, and `acq-rel` operations are explicit and
type-checked for valid load/store/read-modify-write positions. Compiler and JIT reordering must
preserve these edges and the observable order of `ordered-drop`. A safe dynamic synchronization
failure produces a defined trap or typed result, never undefined behavior. Only an unhosted unsafe
boundary may assert unchecked aliasing or synchronization, and the backend records that fact rather
than applying safe-code race assumptions across it.

Module loading performs no ambient user initialization. Initial core modules contain only immutable
constants evaluable by bounded CTFE and declarations. Mutable process, thread, task, or host state is
constructed by an explicit callable that returns an owner and is destroyed by that owner. A future
static-resource facility must specify initialization order, failure, concurrency, and teardown as an
ordinary owned runtime protocol before it can extend this rule.

### Trampolined execution and resumable waits

The VM uses an internal continuation protocol; it does **not** implement general user-visible
`call/cc`. A running program is represented by a typed frame stack (function/module identity,
instruction position, locals/captures, data stack, effect journal, fuel, and source trace). Stepping
that state returns exactly one of:

```text
Continue(thunk)                 execute the next bounded VM slice
Emit(event, thunk)              publish one structured side-effect event, then continue
Await(request, resume_thunk)    persist/schedule the request; do not block a VM or UI thread
Raise(value, provenance)        unwind cleanup to a compatible handler or terminal failure
Complete(values, journal)       commit the transaction
Fail(diagnostic)                discard uncommitted VM-local mutation
```

`Raise` is internal typed control transfer, not automatically a terminal outcome. The VM runs the
verified cleanup path and resumes at a matching handler when one exists. Only an uncaught thrown
value is rendered into a terminal `Fail` diagnostic and aborts the transaction. Traps,
authorization outcomes, cancellation, and resource limits use their protected paths rather than
being converted into catchable thrown values.

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

`fiber<Y,Resume,R>` and `task<R>` are first-class persistent values: their serialized form is a stable
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

The initial Rust runtime may implement Finch `Unique`, `Shared`, and `Weak` carriers with Rust
ownership internally, but that is an implementation of the language contracts above rather than the
language's memory model. Immutable shared objects and uniquely owned builders are useful initial
policies. Cyclic mutable structures should either be excluded initially, use weak edges, or be
placed in an explicitly selected per-runtime tracing arena with safepoints. Do not add a process-wide
collector lock. If tracing collection is introduced, expose it as another ownership policy, prefer
per-runtime/per-arena collection plus immutable cross-arena handles, and document its safepoints.

Unique builders are first-class implementation APIs for strings, bytes/vectors, persistent lists,
diagnostics, and patches. Their consuming `freeze` contract is the one defined by the collection
model above. Compilation and rendering must not repeatedly replace or concatenate whole immutable
strings for incremental construction.

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

A thrown value travels in a compact runtime-owned `ExceptionTransfer` envelope containing its
stable type identity, original throw origin, propagation call/inline origins, task/program identity,
causal link, and suppressed cleanup failures. The payload remains the ordinary source value; error
records do not need to carry compiler spans or repeatedly wrap themselves merely to add context.
Propagation appends compact source-map/frame IDs and formats them only when inspected or rendered.
`rethrow` preserves the original envelope. Throwing a new value from a handler creates a new primary
transfer and retains the handled transfer's causal metadata. It retains the old payload itself only
when source performs a valid copy, retain, or move that leaves an owned value available; provenance
must never implicitly clone a unique payload already consumed by handler bindings.

### Error propagation

- Expected operational alternatives use an ordinary library `result<T,E>` when callers commonly
  inspect or transform them.
- `throw` carries an ordinary typed value and unwinds cleanup to the nearest compatible handler;
  intermediary callers propagate it without result conversion or source annotations.
- A trap is a distinct protected diagnostic and never enters ordinary catch matching.
- An uncaught thrown value aborts the VM transaction and becomes a failed `ExecutionOutcome`; a
  caught value resumes with the handler's compatible result.
- `nothrow` is verified from inferred exceptional successors after handler subtraction.
- Child failures remain structured inside `agent-result`; the master may inspect, retry, summarize,
  or propagate them without scraping text.
- Cancellation and fuel/time/memory exhaustion are distinct stable error kinds.
- Approval denial is not reported as a compiler error; it is an authorization outcome.

Interpreted frames record word IDs and IR offsets. Optimized code uses explicit side exits and
metadata maps. Do not dedicate a permanent machine register to a global error flag, and do not rely
on the Forth return stack to reconstruct optimized/inlined calls.

## Named tests, fixtures, and typed test doubles

Testing is a language-facing declaration and standard-library protocol, not module initialization
or a runner convention that scrapes function names. Every test has a required human-readable name
and a stable fully qualified identity derived from module, nested suite names, and test name.
Duplicate identities are compile errors. Suites provide lexical grouping, inherited tags, and
diagnostic paths; they do not own hidden mutable setup state or impose source-order execution.

Conceptually, both frontends construct the same `TestDeclaration` and `TestSuiteDeclaration` nodes:

```lisp
(test-suite "JSON parser"
  (test "rejects trailing input" (ctx)
    (let ((actual (json/parse "{} junk")))
      (expect ctx actual (matches (err (TrailingInput _)))))))
```

```forth
test-suite: "JSON parser" {
  test: "rejects trailing input" ( S borrow-mut TestContext -- S ) {
    "{} junk" json.parse
    matches{ Error{ TrailingInput{ _ } } } expect
  }
}
```

The exact punctuation may evolve with the paired grammars, but names, lexical grouping, source
origins, and the shared semantic nodes are normative. Co-located test declarations can access their
module's private interface. External test modules are black-box clients and see only published
exports. Production sealing excludes test declarations and test-only evidence from the executable
interface; a separate versioned test artifact links them under the test profile.

A test body is an ordinary checked callable receiving a scoped `TestContext`. It may throw or
suspend according to its inferred contract: ordinary I/O retains Finch's transparent green-task
behavior and requires no `async`/`await` test variant. The runner owns the task, deadline,
cancellation, output capture, capability grants, deterministic seed, and cleanup scope. A return is
success; an uncaught value, trap, leaked owned task/fiber, unmet expectation, timeout, or cleanup
failure is a structured test failure. The diagnostic names the stable test identity and contains
the originating expression spans, matcher explanation/diff, captured output and effect trace, seed,
and suppressed cleanup failures.

`expect` is a hygienic standard-library syntax transform over typed matcher values, not a privileged
comparison opcode. It evaluates the subject once, preserves its source spelling and span, and sends
it with matcher evidence to `TestContext`. Ordinary concepts support equality, ordering, variants,
exceptions, sequences, text diffs, approximate numerics, and user-defined domain matchers. A hard
expectation aborts the current test through the structured test-failure path; an explicit soft
expectation records the failure and continues. Test authors can write ordinary functions around
matchers without losing type checking or diagnostic provenance.

Fixtures are ordinary constructors returning owned values. Lexical ownership, `defer`/scope guards,
and deterministic drop provide setup/teardown, so cleanup runs on success, throw, cancellation, and
timeout without ambient `beforeEach` mutation. A suite may declare a fixture factory shorthand, but
each test invocation receives a fresh result unless the source explicitly requests a shared fixture
owner and synchronization policy. Parameterized tests expand stable case identities from explicit
values; property tests use typed generators plus a recorded seed and shrinking trace. Snapshot/golden
matchers store versioned, reviewable artifacts and never update them merely because a test failed.

Mocks follow the same explicit concept-evidence and dependency-injection model as production code:

- a static concept dependency receives test evidence and remains statically checked/specializable;
- a `dyn Concept` dependency receives a test-owned data pointer plus mock evidence table;
- a concrete callable is tested through an explicit function/record dependency rather than global
  monkey-patching or import replacement;
- typed mock operations record arguments, returns, throws, yields, call order, and ownership
  transfers; impossible calls fail at compile time rather than becoming stringly typed expectations;
- host effects are intercepted at `VmSideEffect`/`VmResume` by a capability-denying fake, scripted
  adapter, or versioned record/replay harness. Clocks, randomness, schedulers, filesystems, and
  provider clients are explicit dependencies or host bindings, never ambient test magic.

Tests are isolated by default: each gets a fresh transaction, task tree, test context, explicit
module-state instances, and no host authority unless requested by its test profile. Independent tests
may run in parallel in any order. A test that genuinely shares an external resource declares a
stable resource key so the runner can serialize or provision it explicitly; relying on incidental
runner order is invalid. Record/replay data is independently validated and cannot grant effects not
present in the test's capability policy.

The minimal runner protocol lists stable identities and metadata without executing module code,
filters by exact identity/tag/module, runs selected tests, emits structured per-test lifecycle events,
and returns a machine-readable summary. Text, editor, TUI, CI, and future embedded runners consume
that same protocol. This preserves the convenience of inline `unittest` blocks, the discoverability
and matchers of Jest, and Finch's typed effects, deterministic cleanup, and parallel isolation.

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

The interpreter and Cranelift are two backends of this same Finch IR, not two languages.
See [Abstraction boundaries](#abstraction-boundaries-do-not-collapse). CLIF never becomes
the handoff from `finch-language`.

Finch IR is the durable semantic and verification boundary. CLIF is target/backend-oriented and
normally a rebuildable compilation artifact. Do not serialize CLIF as the program-exchange ABI or
ask models to generate it. Cranelift consumes only verified concrete typed IR or verified shared
typed IR with explicit evidence parameters; it does not infer source types, resolve concepts, choose
evidence, or repair an invalid generic instantiation. Capability authority is already validated
before lowering, but every runtime shim call remains capability-bound so malformed or stale native
artifacts cannot bypass the broker.

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
interpreted. Shared evidence-passing generics remain eligible for tier 1; tier 2 may selectively
monomorphize layout-dependent or hot evidence/type combinations rather than multiplying code for
every valid instantiation by default.

### Long-term embeddable native tier

A long-term adoption target is a LuaJIT-like embeddable Finch runtime: small enough to ship inside
another application, fast to initialize, inexpensive to call, and able to turn hot CoLisp/Co-Forth
code into native code without requiring the host application to embed the Rust toolchain or Finch's
Rust implementation. This is a distribution and latency goal, not a claim that Finch will reproduce
LuaJIT's tracing architecture or current performance.

Cranelift is a practical first native backend and can itself be embedded. It remains the reference
baseline while language semantics, runtime shims, native metadata, and differential tests stabilize.
After self-hosting, Finch may add a compact baseline machine-code generator written in CoLisp or
Co-Forth. Verified typed stack IR, resolved layouts, explicit control edges, and certified ownership
and effects let that backend encode a deliberately small instruction-selection and register-allocation
surface instead of rebuilding frontend semantics. The initial target should support one architecture
and ABI well, fall back to the interpreter for unsupported operations, and expand only from measured
embedding workloads.

An in-process JIT needs an encoder, relocation/patching support, executable-memory manager, runtime
shim table, source/trap maps, and cache format; it does not require a general-purpose static linker.
AOT and standalone-library output additionally need a constrained object writer or integration with
the platform linker. Finch may eventually provide compact self-hosted implementations of those
pieces, but ELF, Mach-O, PE/COFF, x86-64, AArch64, unwind formats, and calling conventions are
separate correctness surfaces. They must remain target modules behind one backend contract rather
than accumulating target conditionals in the semantic compiler.

The custom backend is successful only if it materially improves cold start, binary size, compile
latency, deployment simplicity, or hot-code performance over the interpreter/Cranelift combination.
Cranelift remains an available fallback and differential oracle until the custom backend passes the
same capability, ownership, exception, cancellation, transaction, and source-origin gates. Native
code always calls the versioned portable runtime ABI, so replacing a backend never changes the host
embedding contract.

### Native ABI and lowering

- Lower verified Finch IR blocks into CLIF blocks and map virtual stack slots to CLIF SSA values.
- Spill only across calls, control-flow merges, suspension points, and register pressure.
- Eliminate `dup`, `swap`, `over`, and local stack shuffles in SSA when possible.
- Lower verified move/borrow state directly; do not rerun source lifetime inference in the backend.
- Emit cleanup blocks and stable borrow/drop/retain/release runtime hooks for owner carriers whose
  operations cannot be inlined, preserving exactly-once destruction across return and unwind.
- Lower verified exceptional successors to explicit native side exits and handler landing blocks;
  handler activation must cover the protected expression before its first call. Preserve the typed
  payload, runtime type identity, and compact provenance envelope without turning every call into a
  source-level `result` value.
- Lower checked arithmetic with explicit overflow/division side exits according to language policy.
- Call stable portable runtime shims for allocation, capability requests, task operations, and complex
  managed-value operations.
- Core owners require no tracing safepoints. An explicit tracing-arena owner extension supplies and
  pays for its own declared safepoint/stack-map ABI without changing ordinary frames and owners.
- Preserve cancellation/fuel polling at verified loop and call boundaries.
- Follow the platform ABI; do not permanently reserve a global error register.

### Future tensor and accelerator lowering

Finch should eventually make efficient matrix/tensor kernels expressible through ordinary CoLisp
and Co-Forth concepts, macros, CTFE, and law evidence rather than introducing a Python-hosted side
language. The target is scripting-level expression with systems-level control when requested:
shape inference, fusion, tiling, vectorization, memory placement, transfer, synchronization, and
specialized native kernels remain inspectable semantic objects.

Do not lower matrix expressions immediately into opaque library calls or scalar stack operations.
Retain a bounded parametric tensor/kernel HIR until shapes, element types, layouts, algebra evidence,
target features, and scheduling choices are known. A macro constructs typed semantic nodes through
the public compiler protocol; it never emits source text, PTX, or unchecked native bytes. A selected
schedule then lowers into an accelerator kernel region under the Finch IR/verifier boundary. That
region explicitly records:

- logical iteration and reduction domains, static dimensions, and guarded dynamic dimensions;
- tensor layout/stride evidence and host, device-global, device-shared, and register address spaces;
- ownership and aliasing of buffers, transfers, views, and temporary storage;
- workgroup/lane mapping, barriers, atomics, asynchronous copies, and divergent control flow;
- required device capabilities, resource limits, exceptional/trap behavior, and source origins.

The accelerator verifier proves bounds or retains guards, rejects cross-space pointer confusion,
checks barrier convergence and shared-memory lifetimes, and validates that host/device effects remain
inside the granted capability profile. Kernel execution is an explicit effect; an optimizer cannot
silently move a computation to a device when transfer, precision, failure, or scheduling would be
observable.

Certified algebraic evidence may drive rewrites before scheduling. Linearity can permit map/reduction
fusion and distribution; associativity can permit reduction trees; identity and annihilator laws can
remove work; Hermitian/unitary refinements can select specialized algorithms. The preceding proof
rules still apply: an unchecked law declaration never authorizes a semantic rewrite, and strict
floating-point or checked-arithmetic behavior forbids transformations that change rounding or traps.

Scheduling is a replaceable compile-time policy rather than semantics. Standard-library policies may
choose tiles, warps/workgroups, vector widths, shared-memory staging, and pipelining from target and
shape evidence. Expert code can provide an explicit schedule or lower-level kernel operations.
Autotuning evaluates only already-verified semantically equivalent candidates under bounded budgets,
then caches the choice by IR, shape/layout constraints, device/driver features, numeric policy, and
compiler version. Dynamic inputs use guards and safe fallback rather than compiling an unbounded
specialization set.

Initial backends should reuse an established GPU toolchain or portable device format and retain a
CPU interpreter/Cranelift oracle. A compact self-hosted backend may later emit one GPU instruction
set directly, but backend replacement must not change kernel semantics or capability enforcement.
Claiming parity with or replacement of a mature tiled-kernel system requires differential numerical
tests, race/bounds diagnostics, profiler/source mapping, representative model kernels, competitive
compile latency and throughput, and measured portability across supported devices. The nearer goal
is a clean Finch kernel substrate on which those results can be earned.

### Errors and deoptimization

Every native code range maps to module/function/IR offset, Forth origin, Lisp origin, and inline
frames. Native and interpreted calls use the same inferred exception summaries and `nothrow`
certificates; the backend consumes those verified facts and does not reinfer them. Thrown values
take exceptional edges into the typed unwinder, while guards and invalid runtime states branch to
separate protected trap stubs. If speculative specialization is later added,
failed guards reconstruct an interpreter frame at a declared deoptimization point. Native and
interpreted execution must produce equivalent handler selection, cleanup ordering, provenance,
diagnostics, and transaction outcomes. The ABI may use a shared side-exit convention or platform
unwind support, but that choice cannot change source semantics and must keep the successful path
small enough to measure against ordinary returns.

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

Native-code generation is not itself the performance goal. Cranelift is a low-latency baseline JIT;
it does not by itself supply the loop optimization, vectorization, and alias analysis behind
optimized Rust/C++ results. Track wall time, allocations, peak resident memory, code size, compile
latency, and dispatch/host-boundary overhead against checked-in interpreter, Cranelift, optimized
Rust, and C++ baselines. The product roadmap requires meaningful wins on measured hot Finch
programs without harming startup or agent latency. Rust/C++-class output remains a separate
language/compiler research target that requires an appropriate optimizing backend or substantial
optimizer work; it is not a Finch Runtime, Brain, or initial Cranelift acceptance gate. Publish
distributions over representative programs and never claim parity from one arithmetic benchmark.

The source-language expressiveness target is comparable to TypeScript for ordinary application
modeling—structural records, closed variants, closures, parametric functions, evidence-based concepts,
modules, reflection/derivation, asynchronous resources, and ergonomic collection/range composition—
without JavaScript prototype mutation or `dynamic` as the routine escape hatch. Both Lisp and
Co-Forth must expose that same typed semantic surface even when Lisp is the more ergonomic human
frontend. Maintain a corpus of equivalent application-sized programs to measure source size,
required annotations, diagnostic quality, first-pass model success, incremental compile latency,
and generated IR as features land. Annotation density is a product metric: common private
application code should read like a scripting language even though module publication and the
independent verifier retain complete static signatures.

The following self-hosting, AOT, native-application, and FFI sections are a separate multi-year
language/compiler track sharing the verified frontend and IR. They must not delay or distort the
smaller product-critical runtime used for model programs, capabilities, effects, diagnostics, and
Brains.

### Eventual self-hosting

Once retained syntax/HIR, the semantic scheduler, CTFE, modules, and AOT ABI are stable, the
frontend and most semantic jobs may themselves be ordinary bounded Finch programs. Rust and other
embedders call that compiler through a small versioned typed service interface; an AOT build may
also export a C-compatible `libfinch_compiler` facade generated from the same declarations used by
ordinary FFI. Keep the reader framing, artifact loader, verifier, effect boundary, and minimal
runtime as a deliberately small stage-0 trusted implementation rather than requiring an existing
self-hosted compiler to validate arbitrary input.

Self-hosting is also the portability boundary for embedding. The staged Co-Forth/CoLisp compiler
image contains the reader, semantic jobs, optimization, and portable IR generation once, rather
than requiring each host language to translate those systems. A versioned C-compatible `libfinch`
ABI exposes opaque runtime, compiler, module, and execution handles; byte/source buffers and owned
diagnostics; compile, verify, interpret, and optional JIT entry points; and the correlated
`VmSideEffect`/`VmResume` callback boundary. It specifies allocator ownership, buffer lifetime,
thread affinity, reentrancy, cancellation, and the rule that exceptions and unwinding never cross
the ABI. Generated C headers and thin language bindings let Rust, Go through cgo, and other native
hosts consume the same staged compiler and runtime image. JIT-generated code calls stable runtime
shims, never the Rust ABI, so Rust remains one implementation and binding rather than the portable
contract.

This removes most compiler duplication, not all platform work. Each supported target still needs a
small stage-0 runtime/verifier build, executable-memory and W^X handling for JIT mode, native ABI and
unwind integration, and an adapter from the host's event loop and effects into the portable resume
protocol. Hosts may choose interpreter-only operation. Bindings should keep crossings coarse—Finch
executes ordinary calls internally and returns to Go or another host primarily for declared effects—
so cgo/callback overhead does not become the cost of every language operation.

The eventual compiler distribution may be a self-bootstrapping staged image. A tiny audited native
stage 0 validates a bounded canonical manifest and loads a minimal Co-Forth compiler module; later
content-addressed stages use only the language/compiler surface exported by the preceding stage,
progressively adding the retained AST, semantic scheduler, CoLisp frontend, concepts/macros,
optimization, and native backend. The image is a manifest-delimited container, not one source
module whose grammar mutates halfway through parsing.

The minimal Co-Forth bootstrap stage may implement semantic jobs as explicit resumable state
machines. Once the preceding stage provides the CoLisp frontend, generalized typed-fiber lowering,
and the scheduler bridge that owns/resumes those handles, the next stage may re-express the same
scheduler and compiler passes as compact direct-style CoLisp fibers; subsequent self-compilation
proves the fiber-written compiler can reproduce itself. This staged rewrite changes source
ergonomics, not dependency semantics or the compiler-service contract. Track compiler source size,
explicit state-machine boilerplate, generated IR size, cold/cached compile latency, and diagnostic
equivalence against the bootstrap implementation.

Manifest entries declare their artifact kind, required compiler/IR/runtime versions, dependencies,
hash, and target where applicable. Each source module crosses its own authoritative parse boundary
under the preceding `StageVerified` compiler-service generation, whose constituent modules remain
individually `ModuleVerified`. Typed-IR entries use the canonical decoder and must
reach `ModuleVerified`; checkpoint entries validate their schema/generation and reverify every
referenced module; native entries are usable only when their derivation/cache key matches
`ModuleVerified` IR and the exact platform ABI. That metadata does not prove source correspondence:
received native bytes are only rebuild hints and must be regenerated locally and, when deterministic,
byte-compared before use. A trusted local cache may reuse bytes only when the local compiler assigned
the key after successful generation and the cache's integrity boundary remains intact; admitting a
remote native builder would explicitly expand the TCB and require a separate attestation policy. A
stage may contain several modules. It becomes publishable only after every module reaches
`ModuleVerified` and a separate stage verifier validates the canonical manifest, referenced module
hashes, dependency closure, composition constraints, compiler-service interface, and native cache
bindings. That verifier alone mints the distinct `StageVerified` publication token. Native bytes
are never a substitute for verified IR.

Stage replacement is transactional publication through a small versioned compiler-service
interface, not in-place mutation of executing machine code. Minting `StageVerified` is the
publication linearization point: it atomically changes the default generation used by subsequently
created root compilation transactions. Each root and every descendant
job/frame/continuation remain bound to one
immutable compiler-service generation, including children created after a newer generation becomes
default. Durable suspension persists that generation hash and pins its modules, dependencies, and
native artifacts across restart. Reclamation waits until no live or durable reference remains. A
failed decode, parse, verification, compilation, or publication leaves the prior default active.
This permits the compiler to interpret early functionality, JIT later functionality, and replace
components as it boots without making scheduling order or partially installed code observable.

ELF may be one Linux packaging target, with equivalent Mach-O, PE, library, or portable-image
containers elsewhere; the language contract is the embedded staged manifest rather than an object-
file format. Release artifacts may carry verified native code or a checkpointed compiler image for
fast startup, but retain hashes of the canonical source/IR and must be reproducibly rebuildable from
stage 0. Measure cold bootstrap, cached bootstrap, per-stage compile time, peak retained state, and
stage replacement latency so self-hosting remains a speed feature rather than ceremony.

Bootstrap reproducibility is mandatory, but a hash proves integrity rather than source
correspondence or publisher authenticity. The genesis TCB must be explicit: either stage 0 includes
a separately audited minimal source-to-IR seed translator, or a small canonical seed IR is admitted
and audited as part of the TCB. Signatures/provenance identify who published an artifact; they do not
prove what source produced it. Check in a content-addressed verified compiler artifact, use the
audited seed to compile Finch compiler source into stage 1, use stage 1 to produce stage 2, and
require normalized stage-1/stage-2 IR or native artifacts to agree. Release trust claims additionally
require diverse double compilation, or an equivalent independently implemented source-
correspondence procedure, against that audited seed path to address a compromised self-reproducing
compiler. Record compiler source, module graph, runtime ABI, target, dependencies, publisher
provenance, and all artifact hashes. Self-hosting must not introduce a privileged AST, type, CTFE,
or code-generation path unavailable to the inspectable language modules it exercises.

### Later AOT compiler target

After the interpreter contract and JIT differential gates are stable, the same verified pipeline
may expose a separate `finchc` target. Pure programs may link a minimal runtime and produce ordinary
standalone executables. Programs with host effects instead link the portable
`VmSideEffect`/`VmResume` ABI and require a capability-providing embedder. Both modes consume the
same span-preserving AST/parametric HIR, dependency scheduler, CTFE/evidence/specialization cache,
verified stack IR, and source maps; there is no AOT-only source language or trusted model-authored
CLIF.
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
explicit unsafe native-extension boundary in an unhosted profile; producing an AOT binary does not
silently enable unsafe execution or grant host authority.

The asynchronous host is selected through a narrow reactor/scheduler interface rather than being
hard-wired to Tokio or one operating-system poller. A standalone service may let the Finch runtime
own the loop; a Cocoa, Win32, GTK, game, or existing C application may instead own the main thread
and supply timers, readiness registration, wakeups, and event delivery. Native callbacks enqueue a
typed correlated resumption onto that scheduler and do not re-enter arbitrary VM frames directly.
This keeps continuation ordering, cancellation, and execute-once effect records intact when the
host loop is swapped.

Later C interoperability should use versioned typed `extern` declarations and generated ABI shims.
Safe wrappers describe argument/result layout, ownership, callback lifetime, thread affinity, and
effects; they never expose a raw owning pointer, and opaque C pointers remain generation-checked
resources or explicit foreign ownership carriers. Calling an unverified symbol,
passing a raw pointer/integer descriptor, variadic calls, and unchecked shared-memory access require
an explicit unsafe boundary admitted only by an unhosted profile. The same declarations feed
interpreter bindings and Cranelift AOT lowering so FFI does not become a second language semantic
path. Hosted profiles may call only safe host wrappers whose implementation is outside the language
sandbox and whose typed effect contract remains independently authorized.

## Implementation work packages

This is a dependency/acceptance map, not current implementation status. Phases 1–6 already have
substantial implementations; the canonical checked/unchecked status and remaining gates live in
`TODO.md`. Items below describe contracts that must still be true at each exit, not a claim that
the phase has not started.

### Phase 0: Freeze contracts and fixtures

- Reconcile the existing value/signature/capability/error/Lisp/Co-Forth specifications with the
  implementation and remove stale duplicate contracts.
- Add canonical language artifact directories and version fields.
- Capture existing useful Forth/Lisp programs as migration and conformance fixtures.
- Replay retained real provider outputs without execution and publish failure/repair categories by
  provider/model before changing declaration order, annotation requirements, or default syntax.
- Inventory every builtin and assign its current cell effect, intended typed signature, effects,
  suspension behavior, and migration status.

Current evidence (2026-08-24): opt-in versioned source-bearing capture and non-executing replay are
implemented for interactive, one-shot, and named-Brain provider responses. Captures retain exact
provider/model identity and the reducible promoted-function compiler context, but not operand
stacks, grants, pending effects, or host resources. The first checked-in source-free report records
11/11 verified programs from `grok-code-fast-1` when given the bounded canonical language package:
eight isolated language tasks (including loops and typed records), two stateful turns sharing a
committed typed word, and one deterministic repair from raw prose plus the real structured
diagnostic. This is a smoke baseline, not
representative corpus completion: additional providers, broader natural multi-turn traffic,
module/import context, and annotation/source-order cases remain required.

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
- Implement ordinary library `option`/`result` variants on the general closed-variant matcher;
  neither receives compiler-owned propagation behavior.
- Add thrown-value envelopes, bounded exception inference, `nothrow` verification, handler regions,
  pattern-binding catch shorthand, and automatic unmatched propagation.
- Add `exit`, `success`, and `failure` scope guards on the same lexical cleanup stack as deterministic
  drops, with shared cleanup-block lowering and primary/suppressed failure preservation.
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

### Phase 11: Self-hosted compact native backend

- Freeze the portable backend/runtime-shim contract using the interpreter and Cranelift evidence.
- Implement one measured architecture/ABI subset in CoLisp or Co-Forth, with interpreter fallback
  for unsupported verified IR.
- Add the encoder, relocations, executable-memory manager, source/trap maps, and cache invalidation
  needed for an in-process JIT before considering a self-hosted object writer.
- Keep target ABIs and object formats in independent modules and compare generated programs against
  both the interpreter and Cranelift.
- Demonstrate a materially smaller or faster C/Go embedding on representative workloads before
  widening target coverage.

Exit: an application can embed a compact Finch-owned native tier through the unchanged `libfinch`
ABI, with measured benefit and safe fallback, without depending on Rust or Cranelift at deployment.

## Testing strategy

Every phase adds tests at the layer where its invariant is enforced:

- parser and source-span golden tests for both syntaxes;
- submission-envelope tests proving the explicit language tag is canonical and the first-byte rule
  applies only to untagged compact provider streams;
- parse/elaboration pipeline tests that vary chunking and valid job schedules while producing
  byte-equivalent interfaces, IR, source origins, and deterministically ordered diagnostics,
  including macro-generated declarations after EOF and distinct `FunctionCertified`, `ModuleSealed`,
  and `ModuleVerified` boundaries that make pre-verification execution unrepresentable;
- foreign-frontend tests that use the versioned builder protocol without source-to-source text or
  direct HIR construction, preserve original spans through diagnostics, and produce IR equivalent
  to native frontends;
- module-import tests for module/function/block scope, visibility only after a local declaration,
  whole/qualified/selective/renamed bindings, direct-local precedence, same-scope ambiguity,
  explicit evidence selection, macro/CTFE phases, branch lexical scope, non-reexport of local
  imports, immutable dependency hashing/cycles, one parse/job under repeated and concurrent imports,
  cache eviction/reload equivalence, precise invalidation, independent artifact validation, and
  absence of runtime initialization or authority;
- initialization tests proving repeated imports construct no runtime state, explicit instances have
  independent lifetimes, shared `Once<T>` owners initialize exactly once under contention, recursive
  initialization diagnoses a cycle, each failure/cancellation policy is deterministic, effects and
  suspension remain visible, and final state drops with its owner rather than module unloading;
- language-test tests proving stable mandatory names and discovery without execution, production
  artifact exclusion, private versus black-box visibility, typed custom matchers and single subject
  evaluation, fresh owned fixtures and cleanup on every exit, static/dynamic concept mocks, typed
  host-effect interception, deterministic seeds/shrinking/snapshots, suspending tests without a
  distinct async form, parallel isolation, explicit shared-resource serialization, and equivalent
  structured lifecycle/results from text, TUI, CI, and embedded runners;
- type inference, value-restriction, no-cross-binding-back-solving, stack-row, branch-merge, and
  loop-invariant tests;
- paired CoLisp/Co-Forth grammar fixtures for every parity-ledger row, each comparing semantic nodes,
  accepted IR/results, and the stable diagnostic for its principal invalid form;
- paired text/collection literal fixtures for array, vector, list, bytes, builders, views, indexing,
  slicing, spread, and freeze, including empty/mixed inference diagnostics and exactly-once
  left-to-right element evaluation;
- cross-length and cross-representation sequence equality/order/hash tests proving no allocation or
  materialization, static unequal-length elimination, dynamic length checking, element-evidence
  selection, map-key consistency, and refusal to compare potentially infinite/effectful ranges;
- text tests for exact UTF-8/scalar equality, invalid decoding, scalar-boundary slicing, distinct
  byte/scalar/grapheme units, versioned normalization/collation, raw delimiters and escapes, absence
  of ambiguous integer string indexing, and constant-pattern dispatch with collision checks;
- operator tests proving operand-directed evidence selection, generated symmetric adapters,
  rejection of ambiguous defaults and accidental ordered reversal, derivation of inequality and
  ordering relations, explicit alternate-policy selection, and identical CoLisp/Co-Forth lowering;
- law-evidence tests distinguishing relational symmetry from operational commutativity, exercising
  associativity/distributivity/idempotence/involution and anti-homomorphism wiring, and proving an
  unchecked or false user law cannot authorize an optimization;
- refined-mathematical-value tests for linear, Hermitian, and unitary operators, including runtime
  validation failure, property-preserving composition, mutation invalidation, certified specialized
  lowering, and strict floating-point/checked-arithmetic counterexamples;
- segmented-text tests comparing every flat/rope/subslice chunking combination without flattening,
  including unequal known lengths, cross-chunk boundaries, identical-owner short circuits,
  allocation-free equality, and segmentation-independent hashes;
- ownership/ABI tests proving array/vector/bytes-to-slice calls are zero-copy, escaped views fail,
  vector growth cannot overlap a borrow, builder freeze consumes and may reuse storage, owning
  conversions are never implicit, C-string interior nul is handled explicitly, and native managed
  layouts never cross C or stable ABIs;
- parameter-pack tests for empty and heterogeneous packs, explicit Co-Forth `args{}` boundaries,
  per-element ownership and left-to-right effects, expansion limits, fixed-signature lowering,
  overload precedence, tuple reification, and rejection from first-class ABI positions before
  instantiation;
- runtime-rest tests for borrowed versus owned escape behavior, explicit spread, homogeneous type
  checking, fixed ABI representation, and refusal to consume an inferred ambient stack suffix;
- unhosted C-varargs tests for default promotions, explicit promoted types, unsupported managed or
  nontrivial-owner values, target ABI classification, fixed-call-site thunks, and hosted rejection;
- concept mapping, associated-output, coherence, shared/static-specialized/dynamic dispatch
  equivalence, canonical receiver adapters for member/static/free/delegated callables, view-carried
  evidence without record mutation, explicit-import/default evidence coherence, sealed selected
  evidence identities, and runtime-factory tests;
- derive-macro tests proving generated explicit concept evidence, hygienic same-named operations,
  concept-qualified static selection, named same-concept ambiguity resolution, and equivalent
  `dyn` evidence-table dispatch, plus rejection of generated-source reparsing/string mixins and
  provenance-preserving structured declaration composition;
- record-layout tests for native/C/versioned-stable representations, opaque boundaries, inline and
  owned placement of the same record type, one-step safe `.` projection, and rejection of raw or
  ambiguous automatic dereference;
- dynamic-property tests proving explicit opt-in, real-member precedence, indexed collision access,
  absence as `option`/`result`, errors inside the hook remain errors, and missing-property lookup
  never satisfies a concept or becomes dynamic method invocation;
- paired CoLisp/Co-Forth ownership cases for borrowing, unique moves, use-after-move diagnostics,
  shared retain/final release, weak upgrade, destructor ordering, owner variance, and static/dynamic
  carrier evidence, plus explicit `Cow<T>` uniqueness/clone behavior and non-cloneable exclusions;
- owner-address tests proving ordinary carriers cannot relocate during a borrow, stable/pinned
  evidence is required for retained native addresses and self-references, infallible pinning cannot
  allocate or suspend, failed fallible pinning returns the original live owner, and derived raw
  addresses cannot outlive their borrow or cross suspension merely because storage is pinned;
- lifecycle tests deriving and joining trivial/local/ordered drop classes through nested fields,
  variants, arrays, collections, and owner carriers; conservatively including final-pointee cleanup
  in `Shared<T>`; preserving exact-once reverse-order destruction through optimization and unwind;
  rejecting fallible or suspending hooks; and warning on obvious strong-owner cycles without
  imposing tracing on acyclic `Shared` values;
- safety-profile tests proving hosted admission rejects reachable and unreachable unsafe instructions
  and transitive unsafe calls with no approval path, while an unhosted build still requires lexical
  unsafe boundaries and explicit profile admission and cannot infer unrelated host authority;
- closure-capture tests for inferred scoped borrows, `:move`, exact and mixed capture lists,
  copy/unique/shared/weak carriers, mutation and consuming callable receivers, escape/suspension
  diagnostics, context-free nested records, macro-generated free names, and identical semantics
  under stack allocation or closure-environment elimination;
- syntax-object tests proving capture forms retain spans, hygiene, phase and binding identity through
  destructuring/construction, while explicit datum conversion loses those guarantees;
- range-refinement preservation, opaque adaptor result, explicit-erasure, prefix-parser remainder,
  and whole-document trailing-input tests;
- effect derivation, selector normalization, containment, intersection, and adversarial path tests;
- compile-fail fixtures with stable diagnostic codes, primary spans, expansion ancestry, and
  constraint/specialization related spans;
- closed-variant layout/destructuring tests for explicit tags, niche encoding, borrowed payloads,
  moved payloads, invalid discriminants, and stable FFI/persistence representations;
- error-model tests proving `result` remains an ordinary value, exception sets are inferred,
  default callers propagate without annotations, `nothrow` rejects only escaping exceptional
  edges, exhaustive handlers satisfy it, explicit public exception bounds reject widening, and
  implementation narrowing preserves an unchanged declared interface;
- callable-substitution and module-compatibility tests covering ownership/receiver modes, effects,
  exception bounds, suspension contracts, linkage, calling convention, variadicness, and target ABI;
- concurrency litmus tests for spawn/join, ownership transfer, mutexes, actor/channel delivery,
  transaction publication, and each atomic memory order, plus compile-fail data races and
  interpreter/native agreement under permitted reordering;
- module-loading tests proving declarations and CTFE constants perform no ambient runtime
  initialization and mutable state enters and leaves only through explicit owned callables;
- match/catch tests for disjoint-arm reordering, specific-before-general diagnostics, ambiguous and
  shadowed patterns, `as` binding, partial catch propagation, and ordinary-match exhaustiveness;
- unwind tests for cleanup-before-catch, reverse scope-guard/drop order, moved cleanup obligations,
  handler-thrown errors, and primary/suppressed diagnostic preservation;
- transaction rollback, stale revision, suspension/resumption, and external-effect journal tests;
- typed-fiber tests for initial start, non-unit resume values, unit-profile `next`/`Done` unwrapping,
  static rejection of raw-handle join, dynamic `try-join` ownership preservation, affine scheduler
  transfer, self/dependency-cycle diagnostics, cancellation, and checkpoint/restart preservation;
- policy-coherence tests proving task joins hide internal park/yield-now events, generators expose
  every semantic yield, invalid wrapper/combinator pairs produce actionable compile errors, and an
  atomic terminal transition never loses or duplicates the consumed handle;
- combinator tests for ordered heterogeneous and homogeneous `join-all`, deterministic
  `cancel-on-error`, terminal-only `race`, loser cleanup, linear `select-complete` remainder
  ownership, `next-any` source identity, homogeneous and explicitly tagged `merge`, stable replay
  tie-breaks, and rotating fairness;
- buffered-producer tests for enqueue-without-rendezvous, item/byte backpressure, oversized items,
  scheduling-quantum fairness, ordered drain-before-terminal behavior, explicit discarding close,
  exactly-once transactional delivery, and stable-ID redelivery across a non-transactional restart;
- scheduler-reaper tests for reservation exhaustion/backpressure, create/drop storms, fair per-origin
  progress, bounded suspending cleanup, restart/replay, and exact-once terminalization;
- child authority attenuation and cross-branch authorization tests;
- serialization compatibility and corrupted IR/manifest rejection tests;
- property tests generating well-typed and deliberately ill-typed IR;
- fuzzing for readers, IR decoder, verifier, selectors, and capability request decoding;
- provider conformance tasks using only the supplied language package;
- interpreter/Lisp-lowering differential tests during migration;
- interpreter/JIT differential tests when the JIT exists, including handler selection, thrown-value
  provenance, cleanup/unwind paths, `nothrow`, and trap/exception separation;
- three-way interpreter/Cranelift/self-hosted-backend differential tests for every supported native
  subset, including relocations, calling conventions, runtime-shim version rejection, W^X transitions,
  source/trap maps, fallback, cancellation, and cache invalidation;
- accelerator tests covering shape/layout inference, guarded dynamic dimensions, address-space and
  alias validation, bounds, barrier convergence, shared-memory lifetime, race diagnostics, law-
  certified fusion/reduction rewrites, strict-numeric counterexamples, bounded autotuning/cache
  invalidation, source/profiler mapping, and differential CPU/device results;
- staged-bootstrap tests for canonical manifest/artifact-kind validation, per-module parse boundaries,
  rejection before every module is `ModuleVerified`, unforgeable `StageVerified` publication,
  failed-publication rollback, root/descendant generation pinning across replacement and restart,
  explicit-job versus CoLisp-fiber semantic equivalence, diverse source-to-stage reproducibility,
  and cached-versus-cold compiler-image equivalence;
- portable-embedding tests in C and Go that load the same staged image through generated bindings,
  compile and verify the same program, compare interpreter/JIT results where JIT is available, and
  exercise diagnostic ownership, effect callbacks, cancellation, threading, and reentrancy rules;
- UI snapshots for approval, denial, compile error, runtime trap, child failure, and revocation;
- platform security tests for symlinks, races, Unicode paths, case sensitivity, and root changes.

CI must test the typed runtime with automation unavailable, enabled-but-ungranted, granted, and
revoked. JIT-enabled and interpreter-only configurations run the same conformance corpus.

## Migration and compatibility policy

Before the language reaches a declared stable release, remove discovered semantic warts rather
than preserving them solely for source compatibility. Most early programs are expected to be
model-generated, so evolve the specification against a checked-in corpus of LLM-produced programs,
compile failures, macro expansions, and diagnostic expectations; use that evidence to improve both
the language and its provider-facing definitions. Release the language as an independently stable
contract only after that corpus and the conformance gates support stabilization. This freedom does
not permit silent reinterpretation: stored programs, checkpoints, IR/native caches, module
interfaces, and provider wire contracts carry explicit language/compiler versions and receive a
defined migration, rejection, or invalidation path whenever semantics change.

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
