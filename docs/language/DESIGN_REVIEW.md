# Finch language design review

## Status

This review is the closure audit between the exploratory
[`FINCH_LANGUAGE_DESIGN.md`](FINCH_LANGUAGE_DESIGN.md) and the normative language specification.
It is not a claim that any named researcher personally reviewed Finch. The named lenses below apply
published ideas and failure modes associated with those researchers and language communities.

The audit has three outcomes:

- **BLOCKER** — the first conforming compiler cannot be implemented deterministically until the
  contract is fixed.
- **REQUIRED** — an implementation can begin, but the milestone cannot close without the contract
  and its conformance fixtures.
- **DEFERRED EXTENSION** — explicitly outside the initial language profile. It is not an implicit
  promise that an implementer may fill in independently.

The formal specification is authoritative for accepted behavior. The design document remains the
rationale and decision history; examples marked illustrative there do not create new syntax.

A second consistency pass on 2026-10-01 reopened the implementation-readiness question. A third
artifact pass checked in the initial formalization required by findings 24, 32, and 35 and then
audited the new artifacts against the prose. A hostile panel then found additional policy and
formalization defects; they are recorded below rather than hidden behind the artifact count.
A fourth pass on 2026-10-03 made the two largest artifacts executable instead of descriptive. Both
normative grammars are now interpreted directly against an accept/reject corpus, and a reference
machine executes programs only by interpreting the rule programs in `semantics/transitions.json`.
Executing them exposed the defects recorded as findings 57 through 68. All 36 transition rules are
executable under the coverage definition, with 132 execution vectors and 27 static rejections.

What is still open after that pass, in one place:

| Finding | What remains | Kind |
|---|---|---|
| 47 | no independent consumer executes the generated prelude signature AST | proof gap |
| 48, 54 | dated benchmark measurements and CI enforcement of the performance budgets | implementation gate |
| 52 | callback attachment, revocation, threading, and reentrancy lifecycle fixtures | fixtures |
| 53 | stable payload type/version/data bound to sealed interface and layout identity | formalization |
| 56 | a vector for cancelling a child parked mid-run; the task combinators | fixtures |
| 69 | reflection record shapes (`FunctionSpec` and the rest) are opaque type names | formalization |
| 77 | multidimensional fixed arrays: contiguous layout, per-dimension bounds checks, equality | formalization |
| 76 | the REPL use: parked turns; the engine axis has no artifact (manifest, admission, turns, compile-time authority, direct binding done) | formalization |
| 75 | every open question for the owner and every unfinished piece, in one list | index |
| 74 | target constraints as compiler input: target-sized `int`, per-target specialization | decided in outline |
| 85 | stackless state machine lowering for fibers, generators, and suspending tasks | decided |
| 84 | diamond dependency resolution, isolated feature flags, and semantic interface compatibility | decided |
| 83 | self-contained script manifests, string locator imports, and decentralized dependencies | decided |
| 82 | method-call syntax (concepts first, then free functions) and destructuring with private fields and drop hooks | decided and specified; not executable |
| 81 | generators as ranges with `reply`; cancel on drop; `start`; a running task keeps its own result object alive; unread failures go to the host | core implemented; `defer`/`spawn`/`foreach` open |
| 80 | local types that can be returned (Voldemort types): wanted, not in the executable core | language decision recorded |
| 79 | aggregate results returned in caller storage through a hidden pointer | decided |
| 78 | hostile review round: 19 defects fixed; handle-drop cleanup, resume binding rule, caught-value type open | fixed, three decisions open |
| 72 | IR version 6: typed verifier judgments, `Shared` and generics | partial |
| 71 | positions that force compile-time evaluation; which values may become constants | language decisions |
| 68 | runtime areas with no rule or vector yet, ordered by how much interpreter rework a late change would cost | hole map |
| 66 | three surface-consistency questions found by execution | language decisions, non-blocking |

Findings 48 and 54 cannot be closed by a specification: they need a measured implementation. The
static semantics (types, ownership, effects, evidence) are specified in prose and by the generated
prelude contracts, but have no executable checker or vectors beyond the static-rejection set; that
gap is recorded under finding 50.

## Review lenses

| Lens | Question applied to Finch |
|---|---|
| Steele/Clinger, Scheme | Are tail position and unbounded tail recursion semantic properties rather than optimizer folklore? |
| Milner/Wright, ML | Does generalization remain sound in the presence of allocation, mutation, effects, and staged evaluation? |
| Leijen, Koka | Is the effect row an algebra with precise introduction, union, masking, and elimination rules? |
| Jung/Dreyer et al., RustBelt | Which safe abstractions rely on unsafe library code, and what obligation makes each extension sound? |
| Matsakis/Rust ownership practice | Are places, loans, reborrows, aggregate consumption, suspension, and thread transfer precise enough to check? |
| Flatt/Racket | Do syntax objects, scopes, phases, introduction, and use-site references define hygiene without textual reparse? |
| Wadler/type-class and Rust coherence practice | Is evidence resolution deterministic under separate compilation and dependency growth? |
| Dijkstra/Hoare | Can each verifier claim be stated as a local invariant with an actionable counterexample? |
| WebAssembly Component Model | Are resources, ownership transfer, traps, reentrancy, and async host crossings explicit ABI state? |
| Compiler implementer/self-hosting | Is every surface form parseable once and lowerable through one deterministic, versioned path? |

Useful precedents include the [Rust coherence rules](https://doc.rust-lang.org/reference/items/implementations.html#trait-implementation-coherence),
[Koka's row-polymorphic effect system](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/paper-20.pdf),
[R7RS proper tail recursion](https://small.r7rs.org/attachment/r7rs.pdf),
[Racket's scope-set syntax model](https://docs.racket-lang.org/reference/syntax-model.html),
[RustBelt's safe-extension obligations](https://people.mpi-sws.org/~dreyer/papers/rustbelt/paper.pdf),
and the [WebAssembly Component Model's resource and trampoline invariants](https://github.com/WebAssembly/component-model/blob/main/design/mvp/Explainer.md).

## Closure ledger

### 1. Normative language boundary and grammar — RESOLVED

**Finding.** The design mixes real CoLisp, real Co-Forth, typed-IR notation, and explanatory
pseudocode. Several passages explicitly allow punctuation to evolve. An implementer therefore cannot
tell which token stream is conforming or construct a parser oracle from the design alone.

**Decision.** The normative specification owns lexical grammar, surface productions, static and
dynamic semantics, diagnostics that affect portability, and conformance levels. The design document
owns rationale only. Every code block in the specification is either a production, a conforming
example, or explicitly non-normative pseudocode.

**Proof.** Golden reader fixtures cover every token class and production in both frontends; paired
semantic fixtures compare normalized construction events, verified IR, results, and diagnostics.

### 2. `[...]` cannot be selected by parse-success fallback — RESOLVED

**Finding.** Trying a strict JSON parser first and falling back to ordinary CoLisp makes punctuation
change the grammatical category of the entire balanced region. `[1 2]` and `[1, 2]` can become a
tuple and a JSON value respectively; a missing comma can turn one valid program into a different
valid program rather than a local syntax error. It also violates the promise that a reader classifies
tokens without speculative reinterpretation.

**Decision.** In CoLisp, `[...]` is always the ordinary sequence/binding/tuple syntax selected by its
syntactic position. Embedded JSON uses the explicit reader form `json[...]` or `json{...}` and is
parsed by the JSON sub-parser. JSON scalars used as ordinary Finch literals keep their normal Finch
spellings. There is no parse-success fallback.

**Proof.** Inserting or removing JSON punctuation cannot change a non-JSON form's node kind. Invalid
JSON diagnostics remain inside the tagged literal and identify the exact JSON token.

### 3. String and raw-literal grammar — RESOLVED

**Finding.** The design asks the future specification to define exact escapes and delimiters.

**Decision.** Escaped strings support `\\`, `\"`, `\n`, `\r`, `\t`, `\0`, `\xNN`, and
`\u{H...}`. Unicode escapes must denote a scalar value; surrogate code points and values above
`0x10ffff` are errors. Source is UTF-8. Raw strings use `r"..."`, `r#"..."#`, and additional
balanced hashes; contents are literal UTF-8 and terminate only at a quote followed by the matching
hash count. Triple quotes are reader sugar for a raw string with indentation normalization defined
by the specification, not a third string type.

### 4. Typed capture and macro phase ordering — RESOLVED

**Finding.** "A parameter typed `syntax` captures its argument unevaluated" depends on knowing the
callee signature before ordinary call elaboration. Co-Forth's `macro:` still uses a separate
name-registration model. Higher-order calls and ambiguous names otherwise make evaluation behavior
depend on late type resolution.

**Decision.** Syntax transformation calls are a distinct compile-time call node created only after
the callee resolves to a statically known callable whose corresponding parameter is `syntax`.
Arguments for those parameters remain retained syntax nodes; other arguments follow ordinary eager
evaluation. A dynamically selected callable cannot have capturing `syntax` parameters. Co-Forth
uses the same callable contract and an explicit balanced `syntax[ ... ]` argument; `macro:` is
retired. `mixin` is the only splice into a compilation unit and always evaluates its transformer
expression at compile time.

**Proof.** Resolution order, shadowing, higher-order rejection, nested transformation, hygiene, and
CoLisp/Co-Forth parity fixtures produce identical expansion trees.

### 5. Hygiene and semantic values converted back to syntax — RESOLVED

**Finding.** Scope-aware construction is well motivated, but resolved `Type` values, resolved
function identities, fresh identifiers, and keyword atoms do not all have specified constructors.

**Decision.** The compile-time catalog includes:

- `type->syntax(type, context) -> syntax ! comptime`, producing a canonical qualified type node
  carrying the type identity and the requested diagnostic origin;
- `function-spec-of(identifier-syntax) -> FunctionSpec ! comptime`, resolving with the identifier's
  existing scope set and waiting for `BodyTyped`;
- `fresh-name(context, hint) -> syntax ! comptime`, producing a binding identifier with a fresh
  introduction scope; and
- `datum->syntax(context, datum)`, where keyword is a distinct datum kind, so no separate
  `keyword-syntax-of` operation is needed.

Quasiquote splicing is structural and is therefore legal in a record constructor argument list;
the resulting constructor is checked exactly like handwritten syntax.

### 6. Effect-row algebra and local state — RESOLVED

**Finding.** The document calls `!` one effect row but does not fully define row equality, duplicate
labels, variables, union, containment, handler subtraction, or masking. It also says exclusive local
mutation makes a function not pure, which needlessly prevents pure CTFE and generic algorithms that
use uniquely owned scratch state.

**Decision.** Effects are canonical finite rows of typed labels plus at most one row variable.
Labels with identical kind and selector are idempotent; different selectors of one capability kind
remain distinct and use the selector containment relation. Calls union rows. A handler removes only
the handled typed control label. Lexically scoped mutation carries `state<r>` for a fresh region `r`;
the region is masked when no reference, closure, task, exception payload, or result escaping the
scope mentions `r`. A masked computation can satisfy `pure`; mutation reachable through shared or
external state cannot. Capability labels are never masked by ownership.

**Proof.** The spec supplies inference and elimination judgments. Fixtures cover duplicate labels,
open rows, generic propagation, partial exception handling, selector containment, escaping state,
and pure local builders.

### 7. Generalization and staged value restriction — RESOLVED

**Finding.** "Effect-aware value restriction" is directionally correct but not an algorithm.
Staging, fresh state regions, closures, and `! comptime` add cases beyond a syntactic ML value
restriction.

**Decision.** A private binding generalizes only variables not free in the environment when its
initializer's residual effect row is empty after permitted local-state masking and the value does
not capture a loan, capability, task/fiber, mutable cell, or generative compile-time identity.
Public, recursive, FFI, and cycle-participating definitions require explicit quantified signatures.
CTFE does not generalize a value merely because evaluation happened early.

### 8. Ownership checker model — RESOLVED

**Finding.** The design describes outcomes but lacks a checkable model for places, projections,
loans, reborrows, aggregate moves, and control-flow joins. `record-set!`, nested mutable field
projection, closure capture, and suspension require those rules to agree.

**Decision.** The specification defines place paths (`local.field[index]` where the index is statically
known), initialization state per place, shared/exclusive loans, reborrowing, and conservative overlap.
An exclusive loan suspends access through its parent until the reborrow ends. Dynamic indexing makes
the whole indexed aggregate overlap unless disjointness is proven. A control-flow join takes the
least permissive initialization/loan state. No loan crosses suspension, task transfer, durable
storage, or FFI. A source-visible aggregate is fully valid or consumed: moving one field from a live
record is rejected. Explicit ownership-pattern destructuring consumes the entire aggregate and
transfers or drops every field. Partial initialization remains verifier machinery for construction,
destructuring, and cleanup, not a state users can retain.

**Proof.** Add adversarial fixtures for nested projection, branch joins, rejected field moves,
whole-value destructuring, mutation during shared borrow, closure escape, suspension, and cleanup
after partially completed construction/destructuring.

### 9. Safe extension and interior mutation contract — RESOLVED

**Finding.** User-defined ownership carriers may hide raw allocation, but no formal obligation says
when their safe interface is trusted. Shared synchronization and interior mutation are mentioned
without a `Send`/`Sync`-equivalent contract.

**Decision.** Unsafe implementations are separately verified or trusted modules declaring a
versioned safety contract. Safe clients may rely only on that contract. `Transfer<T>` evidence
permits ownership to cross scheduler/OS-thread domains; `Share<T>` evidence permits shared borrows
across them. Interior mutability is available only through a synchronization concept whose unsafe
implementation proves aliasing and data-race invariants. Neither property is inferred solely from
field layout.

### 10. `Weak<T>` lifecycle and upgrade — RESOLVED

**Finding.** Strong-count ordering is specified, but weak-count lifetime and `upgrade` are left open.

**Decision.** A shared control block has atomic strong and weak counts. The weak count includes one
implicit weak reference while the strong count is nonzero. Last-strong release performs an acquire
fence before dropping `T`, then releases the implicit weak. Last-weak release performs an acquire
fence before deallocating the control block. `upgrade` uses a compare/exchange loop that increments
a nonzero strong count; success has acquire semantics and failure is relaxed. Counts never wrap;
overflow is a protected trap. These are semantic lower bounds, not a mandated machine instruction
sequence.

### 11. CoLisp ownership spelling — RESOLVED

**Finding.** The current section is labelled draft and mixes source-level `consume-value`, `value`,
explicit `borrow`, and default borrowing.

**Decision.** CoLisp source parameters have exactly three forms: `(x : T)` for the default readonly
borrow, `(borrow-mut x : T)` for an exclusive borrow, and `(steal x : T)` for ownership transfer.
`consume-value` is typed-IR/Co-Forth operand-cell terminology and is not a CoLisp parameter keyword.
Copy types may be copied at a use site without changing this declaration grammar. Capture entries
retain their explicit `borrow`/`steal`/`retain` policies because capture and parameter defaults are
different decisions.

### 12. Coherence under separate compilation — RESOLVED

**Finding.** One implementation per `(concept, type family)` is insufficient when two independent
modules may both implement a concept and type they do not own.

**Decision.** An implementation is legal only in the module that defines the concept or the module
that defines the outermost nominal type in the implemented type. Built-in types are owned by the
core module. Generated implementations inherit the ownership site of the expansion. The rule is
checked when sealing the defining module; dependency composition still rejects duplicate identities
as defense in depth. Newtype/wrapper records are the explicit escape hatch.

**Proof.** Independently compile two attempted foreign/foreign implementations and reject each at
its declaration, without requiring a future linking conflict.

### 13. Associated types and evidence versions — RESOLVED

**Finding.** Projection spelling, normalization, recursive associated definitions, and
`dynamic-evidence-version` ownership are not settled.

**Decision.** `T::Item` is the canonical associated-type/value projection in both type subgrammars.
Projection requires unique resolved evidence and normalizes with cycle detection and bounded fuel.
Associated definitions may reference earlier parameters and other acyclic projections but not form
an infinite normalization cycle. `dynamic-evidence-version` is removed from implementations.
A `stable-evidence` concept declares `evidence-version N`; its sealed revision owns key retirement
and compatibility. An implementation table records that concept revision automatically.

### 14. Compile-time iteration and mutation — RESOLVED

**Finding.** Parameter-pack iteration exists, integer-range iteration does not, and it is unclear
whether a `ct-foreach` body is compile-time code or generated runtime code.

**Decision.** `ct-range(start, end, step)` constructs a finite compile-time integer range after all
arguments become constants; zero step and arithmetic overflow are errors. `ct-foreach` executes its
body at compile time under CTFE fuel/allocation/expansion limits. It may use effects allowed in CTFE,
including maskable local state and curated `! comptime` hooks, but may not perform runtime or host
capability effects. Runtime work must be emitted as syntax and passed through `mixin`, or expressed
with ordinary `foreach`.

### 15. Proper tail calls — RESOLVED

**Finding.** "Where marked by IR" makes a Lisp semantic property depend on an optional lowering
choice.

**Decision.** Every call in specified tail position is lowered as a tail call after required lexical
cleanup. This includes self, mutual, and indirect calls. Tail position is defined structurally for
function bodies, `if`, `match`, `begin`, handlers, and cleanup scopes. Pending cleanup executes
before transfer and cannot retain the current frame. A conforming interpreter and native backend
support an unbounded number of active tail calls; resource exhaustion may still arise from user
data, not call-frame growth.

### 16. Capability-selector expressions — RESOLVED

**Finding.** `join`, `narrow`, and host-root refinement are named but not given surface forms.

**Decision.** Selector expressions are a closed grammar inside capability braces:
`root(name)`, `arg(name)`, string literal, `join(base, relative)`, and
`narrow(base, pattern)`. `join` rejects absolute/traversing right operands. `narrow` intersects the
base selector with the relative pattern and can never widen it. The refined path type uses
`path<ROOT:"pattern">`; `ROOT` may be `workspace`, `project`, `task.output`, or the binding name of
a host-issued `root<K>`, including `host-machine`. The same normalized selector AST is used by
types, effects, grants, and audit records.

### 17. Layout and stable evidence — RESOLVED

**Finding.** The current draft now supplies `repr(native)`, `repr(C)`, and `repr(stable N)`, but a
formal compatibility rule is still needed.

**Decision.** `repr(C)` is target-qualified and never a cross-target wire format.
`repr(stable N)` fixes endianness, scalar encodings, field order, alignment, padding, discriminants,
and validity for a named version and participates in interface hashes. Changing any item requires a
new `N`. Native references, owners, resources, and evidence pointers are forbidden in stable layout
unless represented by a specified stable handle.

### 18. FFI traps, callbacks, resources, and reentrancy — RESOLVED FOR SPECIFICATION

**Finding.** The design states that unwinding cannot cross the portable ABI and that callbacks
enqueue resumptions, but it does not define raw C callback creation, foreign-thread entry, `errno`
lifting, or opaque-handle ownership.

**Decision.** No Finch throw, trap, or cancellation unwinds across a foreign frame. Export shims catch
all three, leave the instance in a defined failed/lockdown state appropriate to the profile, and
return a declared ABI status. A Finch closure becomes a C callback only through an owned
`ForeignCallback` containing a trampoline, stable context pointer, lifetime/revocation token, thread
policy, and declared failure mapping; arbitrary closures cannot cast to function pointers. Calls
from foreign threads attach through the runtime and enqueue a root invocation; they never enter an
existing VM frame. Sentinel/`errno` conversion is an explicit wrapper declaration, not implicit FFI
behavior. Opaque pointers use `ForeignResource<K>`/`resource<K>` with a typed destructor and
generation; raw pointers remain unsafe.

**Scope.** These contracts are frozen now, but raw C interop is not required before the portable ABI
milestone. M1–M3 must reserve the ownership, trap, and scheduling states rather than implement every
adapter.

### 19. Concurrency memory model — RESOLVED

**Finding.** Transaction and task ownership are detailed, but the source-visible data-race model,
atomics, and happens-before relation are not.

**Decision.** Safe code is data-race free. Ordinary mutable places cannot be shared between
concurrent executions. Synchronization types expose versioned atomic/mutex/channel operations that
establish happens-before edges; atomics require an explicit ordering no weaker than the operation's
contract, with sequentially consistent as the scripting default. A data race is impossible in safe
code, not a runtime behavior to define. Unsafe shared memory follows the target memory model and is
outside hosted profiles.

### 20. Traps, exceptions, and cancellation — RESOLVED

**Finding.** The three failure classes are distinguished, but handler/finalizer behavior at all
crossings must be stated as one control-flow rule.

**Decision.** Typed exceptions are catchable and participate in `throws`; traps are protected,
uncatchable program failures; cancellation is a scheduler signal observed only at verifier-marked
safepoints. All three run already-registered, non-suspending, nothrow cleanup in reverse order.
Only typed exceptions enter source `catch`. A cleanup failure is suppressed onto the primary
failure; cleanup cannot replace it. Cancellation and traps never satisfy a `throws` clause.

### 21. Standard-library names versus language semantics — RESOLVED

**Finding.** Examples use plausible but unratified names (`vector-get`, `join`, `map`, `eq?`, and
others), which can accidentally become de facto syntax.

**Decision.** The specification has a versioned required-prelude manifest. A source example may use
only names in that manifest or label the binding locally. Library growth does not alter the grammar.
JSON values use variant/map operations or typed decode; `.` never performs dynamic JSON lookup.
Named test/suite forms have exact paired grammar in the specification rather than "punctuation may
evolve."

### 22. Native SIMD and accelerator policy — DEFERRED EXTENSION

**Finding.** Fixed-size matrices are now expressible, but the prior design had not decided whether
explicit SIMD lane types belonged to the core language.

**Decision.** SIMD types and device kernels are not part of the initial language conformance profile.
M5 may add a versioned `simd<T,N>`/kernel extension after scalar interpreter equivalence and target
capability rules exist. The core language guarantees no particular vectorization, only semantic
equivalence of optimizations.

## Second-pass consistency audit

### 23. Whole aggregates versus partial moves — RESOLVED

**Finding.** Permitting a field move left a source binding in a statically damaged state. That made
the place checker and destructor rules more complicated while weakening the simple user invariant
that a named value is valid until it is consumed.

**Decision.** Finch has no source-visible partial moves. An ownership-pattern destructure consumes
the whole aggregate atomically and transfers or immediately drops every field. Borrowing destructures
leave it intact. Partial initialization remains internal verifier state for construction,
destructuring, and cleanup.

**Proof.** Fixtures reject a projected field move, accept whole-record consuming destructures, drop
discarded fields exactly once, and unwind a destructure interrupted between internal field transfers.

### 24. Executable lexical and core-form grammar — RESOLVED BY ARTIFACT

**Finding.** The specification fixes several tokens and declaration schemas but still does not
define complete numeric/character literals, token boundaries, every type argument, patterns,
expressions, or the two syntaxes' complete core forms. “Normalize to this semantic node” is not a
parser oracle without an exact input production and recovery boundary.

**Decision and artifact.** The normative machine-readable grammars are `grammar/common.json`,
`grammar/colisp.json`, and `grammar/coforth.json`, validated by `grammar/grammar.schema.json` and
`scripts/language/check_language_spec.py`. `schemas/semantic-events.schema.json` fixes the event
envelope and `semantics/canonical-digests.json` fixes domain-separated canonical digests. The S0
implementation gate still requires broad valid/invalid/recovery corpus coverage; it no longer asks
an implementer to invent grammar or event policy.

### 25. Type relation, bottom, joins, and exception subtraction — RESOLVED FROM RATIONALE

**Finding.** Branches must “join result types,” yet version 0.1 does not say whether it has nominal
subtyping, which coercions participate, or how a diverging expression is typed. Exception summaries
are sets of types while catch patterns may be partial, so subtracting a partially matched type is
not defined.

**Decision.** Version 0.1 has the rationale's capability/mutability-driven subtype relation, no
class or implicit record-width subtyping, and an uninhabited `never` type that coerces at joins.
Readonly views and immutable variants may be covariant; mutable views are invariant; callable
inputs are contravariant and results covariant; narrower callable contracts subtype wider ones. A
branch requires one unique least upper bound. Exception sets are subtype antichains, and a type is
removed only when the complete catch matrix exhaustively covers it; partial catches retain it.

### 26. Callable predicates, traps, and cleanup failure — RESOLVED

**Finding.** Earlier drafts exposed `total` and `deterministic` as general callable predicates
without enough value to justify their proof systems. `total` primarily licensed speculation and
pattern-dispatch optimizations; `deterministic` duplicated the referential-transparency promise
ordinary users expect from `pure`. Traps also needed a cleanup boundary.

**Recovered decision.** `pure` means an empty residual effect row after local-state masking and is
orthogonal to throwing/suspension. `nothrow` means an empty escaping antichain;
`non-suspending` means no reachable suspension edge. Language traps unwind; fatal host faults need
not. A guard failure becomes primary on normal exit and suppressed during an existing failure. The
public `total` predicate is retired from 0.1; mechanically proven termination remains an internal
optimization certificate rather than a source contract. Public `deterministic` is also retired;
`pure` entails referential transparency, so every nondeterministic observation is an effect or
explicit input. Execute-once delivery, retry behavior, and ordering remain runtime/effect-protocol
contracts rather than callable predicates.

### 27. Closure capture and overwrite semantics — RESOLVED

**Finding.** The value restriction mentions captured loans and owners, but neither syntax specifies
how a closure selects borrow, exclusive-borrow, copy, or ownership capture. Assignment also does not
say whether the right side is evaluated before the old value is dropped, which matters when either
step fails or aliases the destination.

**Recovered decision.** A proven nonescaping, nonsuspending closure may infer scoped loans. Every
escaping/stored/deferred/erased closure uses the now-frozen `:move`/`captures: move` form or an exact
paired capture list; unique values move, shareable carriers retain explicitly through their evidence,
and no loan is silently promoted.

**Decision.** Assignment evaluates and validates its right side into a temporary first, leaving the
old destination unchanged on failure. Conflicting loans then end, the nothrow/nonsuspending old drop
runs, and the temporary initializes the destination. A result still borrowing from the replaced
value is rejected. Locals, fields, and mutable indexed places use the same order.

### 28. Owner, pinning, and unsafe-extension laws — RESOLVED FROM RATIONALE

**Finding.** The prelude reserves `Owner`, `PinnableOwner`, recovery, and lifecycle concepts plus
`pin`, `get-mut`, and uniqueness operations, but the normative document supplies neither signatures
nor laws. `Transfer`/`Share` safety contracts name the obligation without defining who may implement
them or how negative evidence prevents unsafe automatic derivation.

**Decision.** The specification now carries the rationale's lifecycle/owner/retain/stable-address/
pinning/recovery contracts. Pinning consumes an unloaned owner; infallible pin cannot newly fail;
fallible pin returns the original owner on failure. Lifecycle hooks are bounded, deterministic,
nothrow, nonsuspending, authority-free, and classified for optimization. `Transfer`/`Share` derive
structurally; manual evidence is an unsafe versioned proof obligation.

### 29. Numeric semantics and compile-resource exhaustion — RESOLVED

**Finding.** Overflow and conversions are fixed, but division/remainder, shifts, floating-point
rounding, NaN equality/order/hash, signed zero, and constant-folding equivalence are not. Separately,
fuel exhaustion is described as a compile diagnostic without saying whether it means “ill-typed” or
“this implementation did not allocate enough checking resources.” That makes acceptance depend on
an unstated implementation budget.

**Decision.** Integer division truncates toward zero; remainder follows the dividend; zero division,
the signed-minimum/`-1` case, invalid shifts, and nonrepresentable left shifts trap. Strict IEEE
binary32/64 arithmetic rounds to nearest/ties-even at each declared-width operation, preserves
subnormals, forbids implicit contraction/reassociation/excess precision, and canonicalizes arithmetic
NaN results. IEEE equality remains non-reflexive for NaN, so raw floats cannot provide `HashKey`
without an explicit total-key policy. Cast rounding and exceptional cases are fixed identically for
CTFE, interpreter, native, and ABI execution.

The declarative checker defines semantic validity independently of resources. Exhaustion is a third
`resource-exhausted` outcome, never an ill-typed judgment. The specification declares minimum work,
recursion, instantiation, and memory budgets; exact limits and peak use are result metadata and cache
inputs, and retry under a larger budget does not change program identity.

### 30. Task/fiber/stream failure and checkpoint types — RESOLVED

**Finding.** `task<T>`, `stream<T>`, and `fiber<Y,Resume,R>` do not encode a child's typed exception
set, cancellation terminal state, or checkpoint eligibility. A handle can therefore cross a module
boundary without enough type information to check `join`, `next`, or resume. “Checkpointable
suspension” also overpromises when a frame owns foreign resources, capabilities, loans, or an
in-flight external effect.

**Recovered decision.** The policy wrappers are affine and distinct; cancellation and terminal
transitions consume their handles. `race-and-reap` reaps losers, `select-complete` returns one composite owner
for the remainder, dropped live handles transfer to a bounded reaper, and only persisted tie-breaks
have replay/fairness guarantees. Checkpointing requires separate structural `Checkpointable`
evidence and rejects live loans/raw pointers/unreplayable resources or effects.

**Decision.** The handle types are `task<T,X>`, `stream<T,X>`, and `fiber<Y,Resume,R,X>`, where `X`
has kind `ExceptionSet` and is written `throws<E...>`. There are no exception-erasing short aliases.
`join`, `next`, and fiber stepping rethrow exactly `X`; combinators use canonical exception-set
union. Protected trap/cancellation/authorization/resource outcomes are not smuggled into `X`.
Checkpointability remains independent structural evidence and does not follow merely from the
producer's result or exception types.

### 31. Planned attributes, axioms, derives, and tests lack normative semantics — RESOLVED FROM RATIONALE

**Finding.** The roadmap schedules axioms, attributes, derives, property tests, doubles, and
snapshots, while the normative specification either only reserves a name or omits the feature. The
plan therefore asks implementers to invent semantics from the rationale document.

**Decision.** Axioms are named pure compile-time checks over fully known type/value facts and never
candidate filters. `@Name form` is syntax-transform sugar, including structured declaration
metadata. Generated declarations pass normal checking. Tests are non-initializing declarations in
a separate artifact with stable identities, ordinary owned fixtures, typed matcher evidence,
explicit test doubles, deterministic property-test records, and non-self-updating snapshots.

### 32. Profile, host-mode, identity, and ABI boundaries — POLICY RESOLVED, ABI ARTIFACT INCOMPLETE

**Finding.** The accelerator contradiction is now fixed: it is reserved, not a 0.1 profile. Other
boundaries remain qualitative. “Hosted” and “unhosted” are used without being profiles or execution
modes; module path normalization lacks Unicode/case/symlink rules; `repr(C)` names a target ABI but
not its identity; and the portable ABI has invariants but no wire/schema artifact.

**Recovered decision.** Hosted/unhosted are admission modes, not language profiles. Hosted admission
rejects any transitive unsafe edge; unhosted requires build and policy opt-in and grants no ambient
authority. Module identity derives from the owning manifest root rather than source declarations or
search order.

**Decision.** Logical source components must already be NFC and compare case-sensitively. The loader
binds one physical package root, rejects resolved escape, and rejects a package graph with NFC,
default-case-fold, or physical-file collisions, making host case/symlink behavior unable to select a
different module. A target ABI identity includes architecture, OS, environment, object format, C ABI
revision, pointer width, endianness, scalar layout, aggregate layout algorithm, and calling-
convention set; its canonical digest, not a loose target triple, gates `repr(C)`. The portable ABI
uses its own versioned, target-independent envelope and fixed scalar wire encodings.

**Partial artifact.** `schemas/target-abi.schema.json`, `schemas/target-registry.json`,
`schemas/native-call-abi.schema.json`, and `schemas/portable-message-abi.schema.json` now separate
target identity, native calls, and serialized messages. The checker
rejects unknown registry keys and duplicate admitted tuples. Finding 52 reopens formal closure: the
portable schema still conflates native calls with serialized messages and lacks tagged message,
ownership, replay, callback, and canonical-encoding state machines. Host fixtures alone cannot
repair an ambiguous schema.

### 33. Namespaces, shadowing, and overload selection — RESOLVED

**Finding.** The draft gives one member-resolution order but does not define whether types, values,
modules, concepts, and compile-time bindings occupy one namespace or several. It also mentions
imported overload groups while the rationale rejects general overload resolution. Case sensitivity,
shadowing, duplicate declarations, and ambiguity across lexical imports are not fully specified.

**Recovered decision.** Direct lexical declarations precede imported candidates; inner scopes
precede outer scopes; same-scope import ambiguity is order-independent. Overload ranking uses
explicit arguments and already-known argument types only; expected results and import order never
choose a candidate, and representation/dispatch-only overloads are forbidden.

**Decision.** Finch has module, type, and value lexical namespaces. Concepts share the type
namespace; compile-time transformers share the value namespace because phase does not license a
shadow binding. Syntactic position selects a namespace before lookup. Members remain owned-type
names rather than lexical bindings. Direct duplicates in one scope/namespace are errors; the same
spelling may occur once in different namespaces. Identifiers must already be NFC and compare by
case-sensitive Unicode scalar sequence. Inner direct bindings shadow outer bindings, but an import
cannot hide a direct binding in the same scope; same-scope imported ambiguity is order-independent.

### 34. Erased concept eligibility — RESOLVED FROM RATIONALE

**Finding.** `dyn C` promises a fixed evidence-table ABI, but concepts may have associated values,
generic operations, `Self` in results, value parameters, or operations whose ownership contract
depends on the concrete type. Not every statically usable concept can therefore form one erased
view.

**Decision.** A `dyn` view binds every client-visible associated output and exposes only operations
with one finite layout-independent ABI slot. Generic operations, unbound returned `Self`, layout-
dependent operations, and unbound outputs remain static unless explicitly reified through a fixed
ABI. Erasure, checked recovery, and evidence-subset upcasts are explicit.

### 35. Core dynamic semantics — RESOLVED BY EXECUTION

**Finding.** Evaluation is declared eager and left-to-right, but there is no small-step machine or
equivalent transition table for binding, call, assignment, loops, matching, throw/catch, cleanup,
and tail transfer. IR verification cannot be shown equivalent to source behavior from evaluation
order alone.

**Recovered decision.** The runtime already has the syntax-neutral `Continue`/`Emit`/`Await`/
`Raise`/`Complete`/`Fail` transition protocol, protected trap/cancellation/resource edges, correlated
resume identity, and journal-before-exposure rule. The specification now records it.

**Partial artifact.** `semantics/transitions.json` supplies abstract-machine programs and invariants.
`fixtures/source-to-ir.json` currently exercises literal, addition/comparison calls, conditionals,
binding, sequence, read, assignment, loop, matching, scoped cleanup, throw, catch, rethrow, and terminal-failure
paths. The harness parses both source spellings and requires them to reconstruct the checked AST.
It now derives canonical fixture stack IR, executes it independently, requires an exact one-value
return stack, and compares its complete trace and terminal result with the source evaluator; this
exposed and repaired missing sequence/loop discards in the earlier dead IR. `transition-coverage.json`
is an exact checked partition of executable and pending rules. The source evaluator remains
handwritten and independent of the rule programs; machine exercises skip evaluation operations and
project only selected expected keys. Consequently all 28 rules were pending.

**Closure (2026-10-03).** The handwritten evaluator and the skip-based machine exercises are deleted.
`scripts/language/reference_machine.py` has no per-form logic: it runs a node by interpreting that
form's program from `transitions.json`, and unit tests prove the dependency by mutating a rule
program and observing the trace change. The rule file was rewritten so each program is literally
executable (labels, declared completion, declared branches, and a stated unwinding discipline), and
six rules that the earlier table lacked were added: lambda, fiber construction, yield, spawn, join,
and cancel. `transition-coverage.json` now defines executable as "every instruction run and every
declared branch taken by a vector that asserts the complete trace, terminal, and final state"; all
36 rules meet it. The small fixture stack IR remains as an independent second executor for the 14
vectors it can express and is no longer part of the definition. Remaining limits are stated under
findings 50 and 56, not here.

### 36. Open effect rows lacked source syntax — RESOLVED

**Finding.** The static model requires row variables and generic propagation, while the contract
grammar could spell only concrete requests or `pure`. A higher-order published callable therefore
could not expose its parameter's residual row without inventing syntax.

**Decision.** Generic headers bind `effect e`; `effects<e>` includes that open tail in a contract;
`effects-infer` freezes an inferred, possibly parametric row expression in a sealed interface.
`exceptions X` and `stack S` bind the other non-`Type` generic kinds already present in the kind
system. The common grammar and generated prelude use the same forms.

### 37. Affine stream/fiber advancement lost the successor handle — RESOLVED

**Finding.** The prose required every advance to consume a direct handle and return its sole
successor, but described `stream.next` as returning only `option<T>`. A successful item could not be
followed by another advance without duplicating or hiding the linear handle.

**Decision.** `stream-step<T,X>` is `item(T, stream<T,X>) | done` and
`fiber-step<Y,Resume,R,X>` is `yielded(Y, fiber<Y,Resume,R,X>) | returned(R)`. `next` and fiber
stepping return those closed variants or raise `X`. Terminal variants contain no successor.

### 38. Refined path types were outside the shared type grammar — RESOLVED

**Finding.** Section 14 used `path<ROOT:"pattern">`, but the shared type grammar admitted only
types, constants, and associated bindings as generic arguments.

**Decision.** A `refined-path-argument` is a qualified root, colon, and string pattern. It is valid
only where the expected generic kind is the path selector pair; other uses fail kind checking. The
prose grammar and `grammar/common.json` now agree.

### 39. Required-prelude names lacked enforceable signatures — RESOLVED BY GENERATION

**Finding.** The bootstrap manifest froze names but left signatures and ownership/effect contracts
to future fixtures, so a name match could conceal incompatible implementations.

**Decision and artifact.** `prelude-definitions.json` records kinds, semantic form mappings, and
canonical callable signatures with exception and suspension axes. `spec-prelude.json` is generated
with a source digest; the checker rejects stale output, duplicate operations, missing contracts, or
missing closed axes.

### 40. Generic binders and capability requests were not parser-complete — RESOLVED

**Finding.** The static model used bounded type parameters, inferred outputs, and multiple generic
kinds without giving all of them productions. Separately, the contract prose used parentheses for a
capability request while the selector section described request braces.

**Decision.** Generic entries now cover bounded `Type`, value, effect-row, exception-set, stack-row,
inferred-output, and pack binders; generic implementations carry the same optional header.
Capability requests uniformly use `namespace.operation{name=selector,...}` so request payloads are
lexically distinct from calls. The paired grammar artifacts encode these choices.

### 41. Canonical event attributes and digest domains were too permissive — RESOLVED

**Finding.** An event `kind` initially accepted arbitrary attribute names, allowing two frontends to
hide different semantics in equally schema-valid payloads. The first portable-envelope digest draft
also excluded its payload, so it did not identify the message it claimed to digest.

**Decision and artifact.** `schemas/semantic-event-kinds.json` closes required and optional
attributes for every event kind, and the checker requires exact registry/schema coverage. Portable
ABI digests include the complete validated envelope. Digest tags use an explicit `00` terminator,
and wildcard pointer removal has one defined array-level meaning.

### 42. `select-complete` could lose remainder ownership on typed failure — RESOLVED

**Finding.** If the selected child raised `X` directly, ordinary exception propagation could bypass
the promised return of the remaining handles. The contract could not simultaneously throw and
return their linear owner.

**Decision.** `exception-value<X>` reifies a member of exception set `X`, and
`task-outcome<T,X>` is the ordinary-data variant `returned(T) | raised(exception-value<X>)`.
`task-selection<T,X>` carries that outcome, the selected index, and the sole
`remaining-tasks<T,X>` owner. `select-complete` is `nothrow` but may suspend. Protected
terminal outcomes cancel and reap the remainder before propagation; a caller may explicitly throw a
typed failure only after disposing of or transferring the remainder.

### 43. Affine shared handles were being retained implicitly — RESOLVED

**Finding.** `Shared` and `Weak` were declared move-by-default and non-`Copy`, but stealing calls and
`:move` closure capture were later allowed to retain them implicitly. That duplicated a handle and
changed count/lifetime behavior without an explicit source operation.

**Decision.** A stealing call and move capture transfer every non-`Copy` handle and invalidate the
source. Keeping a strong handle requires `retain`; creating another weak handle requires
`retain-weak`. Capture mode `copy` requires `Copy` evidence; the undefined `clone` spelling is
removed. `retain` and `weaken` remain explicit capture modes.

### 44. Lexer token ownership and literal reachability — RESOLVED

**Finding.** A low-precedence `module_path` token competed with `identifier`, dialect-specific
character forms were parser fragments without lexical coverage, and triple strings were not
reachable from the imported `literal` production. The common `#` token could also win over CoLisp
Boolean and character spellings.

**Decision.** Module paths are parser compositions of NFC identifier components. Each dialect owns
its character token and literal override, including triple strings; CoLisp Boolean and character
tokens outrank the standalone stable-key marker. The shared grammar retains only literals whose
lexical spelling is genuinely common.

### 45. Event-tree identity and fixed-width portable values — RESOLVED

**Finding.** Parent IDs alone did not state which normalized-AST field contained a child, so a
digest could not distinguish reordered semantic roles reliably. The first portable ABI schema also
accepted bare JSON integers and unconstrained encoding strings despite promising fixed-width wire
values.

**Decision and artifact.** Every semantic event now carries canonical child `role` and optional
array `index`, with preorder, parent, contiguity, and span invariants checked. Portable non-Boolean
scalars use exact tagged lowercase-hex encodings; owned results name their release operation; status
and diagnostic presence agree. `fixtures/schema-instances.json` validates canonical interface and
portable messages and pins their domain-separated digests.

### 46. Exception-set parameters were used as value types — RESOLVED

**Finding.** `task-selection<T,X>` claimed to contain `result<T,X>` even though `X` has kind
`ExceptionSet` and `result` requires an error of kind `Type`. Several required-prelude signatures
also declared exception, effect, and suspension parameters as unadorned `Type` parameters.

**Decision.** `exception-value<X>` explicitly reifies a member of an exception set, and
`task-outcome<T,X>` carries either a returned value or that reified exception. Prelude signatures
use `exceptions X`, `effect e`, and `value s : suspension` binders wherever those kinds occur.

### 47. Prelude signatures have structural ASTs but not a complete semantic oracle — PARTIALLY RESOLVED

**Finding.** Generation originally proved only that the checked-in prelude matched its reviewed JSON
source. Balanced-looking signature strings could still contain malformed parameters, evidence,
contract rows, loan origins, selector expressions, or generic kinds.

**Repair and remainder.** The generator now emits structural ASTs for every callable and concept,
including ordinal generic and parameter references, typed bounds and evidence, ownership modes,
associated projections, loan origins, all three contract axes, effect rows, state targets, and the
closed capability-selector grammar. Negative mutations exercise malformed parameters, constructor
arity, cross-kind row use, duplicate axes, concept requirements, opaque selectors, and missing loan
origins. The earlier ad hoc `law ...` signature member, which belonged to neither frontend grammar,
has been removed; named trusted promises now use range-checked expression ASTs. No independent
consumer yet executes the generated signature AST, so that proof gap keeps this finding open; no
language-policy choice remains.

### 48. Formal machinery could accidentally become mandatory runtime machinery — RESOLVED POLICY, PERFORMANCE PROOF REQUIRED

**Finding.** A literal implementation of JSON semantic events, boxed `Continue` objects, runtime
effect rows, unspecialized evidence dictionaries, or portable encoding on internal calls would make
the language slow for reasons unrelated to its semantics. Compile time can also grow badly through
generic instantiation, evidence candidate search, row/exception normalization, CTFE expansion, and
whole-module replay after small edits. Ordered destruction, checked arithmetic, bounds checks,
atomic `Shared` counts, Unicode graphemes, checkpointing, and external-effect journals are genuine
cost centers even in a good implementation.

**Decision.** Schemas and transitions specify logical equivalence, not allocation or serialization
strategy. Static evidence and empty effect machinery may disappear; proven checks may be removed;
JSON, portable encoding, journaling, checkpointing, grapheme work, and atomic reference counting are
pay-for-use boundaries. `Unique` remains the default ownership path. Semantic ordering barriers and
observable lifecycle operations may not be optimized through. `strong-count` is removed from the
required 0.1 prelude: a diagnostic library may expose it for a concrete carrier, but portable source
cannot observe owner counts and thereby force all implementations to preserve that representation.

**Required proof.** S0–M5 benchmarks must separately track cold and incremental compile latency,
peak compiler memory, generic instantiations and emitted-code growth, interpreter dispatch, optimized
native throughput, allocation count, `Unique` versus `Shared` cost, exception/suspension setup,
effect-journal cost, and portable-ABI encode/decode throughput. Until dated budgets and regressions
exist, Finch may claim semantic capability but not fast compilation, zero-cost abstraction, or
competitive runtime performance.

### 49. Published reader artifacts contradicted their own surface language — RESOLVED BY EXECUTION

**Finding.** Embedded JSON reused Finch numbers and escapes; Co-Forth could not tokenize `name:`;
parenthesized comments lost to delimiter precedence; postfix productions were directly
left-recursive; syntax quote excluded operators; reader decorators were not expressions; standalone
CoLisp `catch` escaped its `try` context; and the prose omitted negative constants. The checker only
resolved grammar symbol names and did not execute either normative grammar.

**Repair.** JSON now has dedicated RFC tokens, labels normalize explicitly, punctuation and exact
terminal precedence are stated, comment/signature lexical modes are explicit, postfix stack words
are non-recursive, quotation and decorator reachability are paired, catch is contextual, and the
constant prose matches the artifact. Fiber/yield and record construction now have paired forms.

**Further repair and remainder.** Raw, triple, and Co-Forth compatibility strings now require
bounded single-pass scanners, and Co-Forth character escapes match the prose. Every declared
reserved word is now required to name a real grammar terminal, with a mutation test for typos;
other word terminals remain contextual as the notation promises.

**Closure (2026-10-03).** `scripts/language/grammar_engine.py` interprets `common.json` plus one
frontend grammar with no production restated in code. `fixtures/grammar-corpus.json` holds 29
accepted and 17 rejected sources; the checker requires every production of both frontends to be
used by an accepted source and every rejection to report its stated code and byte offset. Execution
vectors are read through the same engine, and their semantic events carry real byte spans from each
reader. The defects this execution found are findings 57 and 58.

### 50. Type/effect contracts admitted impossible or unknowable programs — PARTIALLY RESOLVED

**Finding.** Abstract requirements used inference markers without bodies, borrow-return origins had
no syntax, borrow-based operations could suspend, numeric value conversion lifted through covariant
borrows as subtyping, duplicate `Drop`/`Lifecycle` and `Sequence`/`Range` concepts competed, pinning
bounds and failure ownership were implicit, and `spawn` discarded its producer effect row.

**Repair.** Abstract requirements must use fixed or explicit row parameters; `returns-loan` records
one parameter ordinal; borrow-based range/access operations are non-suspending; numeric and variant
widening are owned-value coercions; `Lifecycle` and `Range` are the sole abstractions; pinning binds a
stable-address owner and exposes owner recovery; spawn is charged the captured row; operator and
callback contracts are explicit. Custom lifecycle/Copy admission requires verifier/trusted proof.

**Progress (2026-10-03).** Ownership now has an executable static reference for the executable
core. `scripts/language/static_check.py` decides copy, move, or borrow for every place read from
types and binding kinds, tracks moved bindings across branches and loops, and rejects 21 of the 23
static-rejection programs without executing them (the other two fail earlier, in resolution). The
checker requires its decision to equal what the reference machine does dynamically on every read
the vectors execute (73 reads). It infers only as much type as ownership needs; it is not the type
checker, and it does not yet place drops.

**Open remainder.** No artifact executes the remaining static judgments. Types, ownership, effect rows,
exception sets, and evidence resolution are specified in prose and in the generated prelude
contracts only. The execution vectors are untyped programs chosen to be well-typed by inspection,
and `static-rejections.json` covers only the errors a dynamic machine can observe as a violated
invariant. An executable static checker with its own accept/reject vectors is the largest remaining
piece of specification work.

**Further repair.** `Contiguous`/`KnownLength` now carry trusted laws, record mutation is a checked
intrinsic, `freeze` uses `Freezable` with an associated output, regions have binders and canonical
substitution, resumable payloads require owned-no-loan admissibility, exception widening is explicit
and handles are invariant, and compile-time `syntax` is a `Type` distinct from kind `Syntax`.
Finding 47's real signature AST and negative corpus must still enforce every one of these rules.

### 51. Runtime fixtures did not execute the claimed semantics — RESOLVED

**Finding.** Source fixtures originally ran only a separate evaluator, fixture IR was dead data,
machine exercises skipped evaluation operations, expected results projected selected fields, and one
happy path was counted as complete rule coverage. Cleanup failure/suppression, partial record
initialization, replay correlation, terminal linearization, and concurrency combinators lacked
executable state machines.

**Repair and remainder.** The 10 paired source fixtures now lower their normalized AST to canonical
fixture IR and execute that IR, checking exact traces, terminals, branch targets, stack underflow,
and a one-value return stack. All 28 rules remain pending in `transition-coverage.json` because this
limited lowering is not execution of `transitions.json`. A closed instruction schema rejects unknown
or malformed fixture IR, and a control-flow verifier checks every branch/match target, stack height,
local initialization, handler transfer, cleanup stack delta, and one-value return—including
unselected paths. It does not yet certify operand types, ownership, effects, or exception sets.
Finding 35 remains open until canonical rule execution, full-state comparison,
and hostile branch/restart/race coverage exist. In particular,
effect requests require execution/generation/sequence identity plus durable acknowledgement rules,
and `fiber-step` must return its public `yielded`/`returned` variant rather than a VM terminal shape.

**Closure (2026-10-03).** `fixtures/execution-vectors.json` replaces both earlier fixture files. Its
100 vectors assert the complete trace, terminal, and final state (journal, drop sequence, host
acceptance log, reaper, transaction, peak frames) and cover the cases this finding named: cleanup
failure as primary and as suppressed under failure and under cancellation, partial record
initialization, loop and frame transfers through guards and handlers, cancellation at a safepoint,
host resumes that are stale, out of order, unsolicited, duplicated, conflicting, and post-terminal,
and fiber steps that yield, return, raise, trap, and await. Effect and resume acceptance runs
through the replay automaton, and `step` returns `yielded(value, successor)` or `returned(value)`.

### 52. Portable ABI mixed native calls with serialized envelopes — PARTIALLY RESOLVED

**Finding.** One schema conflated native borrowed pointer/length FFI with JSON effect/RPC transport,
lacked a message discriminator, applied ownership/release to inline primitives, encoded fixed-width
identities as unsafe JSON integers, and had no callback/thread/revocation or replay acceptance state
machine. Target calling-convention labels also failed to distinguish Darwin arm64 from generic
AAPCS64.

**Repair.** Native-call and portable-message schemas are separate. Messages are tagged
call/result/effect/resume variants with inline versus owned results and exact-width hex identities;
native descriptors bind target/layout/calling convention and callback policy. Darwin arm64 has
distinct registry identities. Canonical JSON rejects duplicate/NFC-colliding keys and surrogates,
and the checker pins scalar widths. Local calls are forbidden from paying JSON, hex, or SHA costs.

**Further repair and remainder.** A machine-readable generation/sequence replay automaton now
executes duplicate, conflict, stale-generation, skipped-sequence, and post-terminal fixtures. A
sealed operation table checks message/native arity, ownership, type, release, and callback policy.
Complete callback attachment/revocation lifecycle fixtures remain.

### 53. Interface and semantic identities were too weak for independent implementations — PARTIALLY RESOLVED

**Finding.** Module interfaces store signatures as strings and cannot bind nominal identity,
evidence/layout, effect axes, or target ABI digests structurally. Semantic events accept broad
attribute blobs, fixture sources have fabricated zero spans/digests, and paired digests are not
computed from two independent readers. Stable-record payloads are not bound to interface/layout
identity.

**Repair and remainder.** Module interfaces now carry a structured signature/type/contract/evidence
AST, separate contract/body/inline/CTFE identities, used-export dependency fingerprints, and layout/
target digests. Semantic-event envelopes now bind a real SHA-256 and UTF-8 byte length for their
source, and span bounds/source identity are checked.

**Further repair (2026-10-03).** Each reader now emits its own event stream with real byte spans; a
child's span must lie within its parent's and only a synthesized unit literal may be empty. The two
streams are digested independently and must agree. Structured attributes such as patterns now
participate in the digest (finding 62). Still bind stable payload type/version/data to the sealed
interface and layout contract.

### 54. Compile and runtime speed promises lacked enforceable boundaries — RESOLVED POLICY, BUDGETS REQUIRED

**Finding.** Mandatory per-tuple generic bodies and whole-interface invalidation could explode code
and incremental rebuilds. Universal transaction/journal language, loop polling, blocking `race-and-reap`
cleanup, hidden `freeze` allocation, cache keys containing resource budgets, and unrestricted
specialization could impose avoidable taxes. Existing benchmark prose had no numeric budgets.

**Decision.** A verified shared parametric body is the generic baseline and specialization is
bounded and optional. Dependency fingerprints include only actually used exported contracts;
body/inline/CTFE identities are separate. Ephemeral mutation needs no durable transaction, and
journals/checkpoints are lazy pay-for-use boundaries. Non-suspending direct calls allocate no
continuation; cancellation checks may be hoisted or amortized while preserving bounded response;
successful semantic cache identities exclude administrative budgets.

**Acceptance artifact and required proof.** `semantics/performance-budgets.json` now fixes numeric
cold/incremental compile, RSS, generic-growth, cancellation-response, and exact zero-tax boundaries
plus the mandatory measurement manifest. Range refinements state complexity, and `race-and-reap`
exposes loser-cleanup latency in its name. Dated measurements and CI enforcement are still absent,
so no speed claim is yet earned.

### 55. Program roots and ordinary control flow were not defined — RESOLVED

**Finding.** Both module grammars accepted top-level expressions without saying whether they run,
while imports promise no initialization. Loops had neither `break` nor `continue`, and functions had
no early `return`. Tail recursion can encode those exits but is hostile everyday ergonomics and
leaves cleanup edges implicit.

**Decision.** The source envelope selects library, executable, submission, or script before parsing.
Libraries and executables are declaration-only; executables name one exported `main` in the
manifest. Submission/script expressions lower to one policy-checked implicit entry callable.
Paired `return`, `break`, and `continue` forms are cleanup-aware and reject missing lexical targets.
Hostile nested-scope, cleanup-failure, and cancellation fixtures remain required under Finding 51.

**Artifact.** Each frontend grammar publishes an exact `root_entrypoints` map. Library/executable
roots select the declaration-only `module` production, while submission/script roots select the
executing `submission` production; no parse-success fallback may cross that boundary.

**Closure (2026-10-03).** Vectors execute `break`, `continue`, and `return` through nested scopes,
handlers, and call frames, including a guard that fails during a `break`. Static rejections cover a
missing lexical target, a transfer out of a guard body, and `return` in the implicit entry callable.
The corpus proves a library root rejects an executing expression in both frontends.

### 56. `spawn` had a type but no normative linearization or authority rule — RESOLVED POLICY, PARTIAL FIXTURES

**Finding.** The required prelude exposed `spawn`, while the normative concurrency text specified
task handles only after construction. Two runtimes could disagree about whether a child runs before
its handle exists, who cleans a closure when scheduler admission fails, whether cancellation can
orphan the handoff, and which host grant the child receives.

**Decision.** `spawn` reserves capacity and a child identity before one atomic handoff that moves the
closure, publishes the sole handle, and makes the child runnable. Pre-handoff failure creates no
child and retains the parent's cleanup obligation; post-handoff scheduling may precede the parent's
next instruction. The child grant is the intersection of the parent's live grant, the callable's
declared capability row, and host child policy. Construction is non-suspending, executes no child
body or external effect, and pays no serialization, checkpoint, portable-message, or parent-
continuation tax for a nondurable local task. Hostile capacity/cancellation/revocation fixtures are
still required.

**Fixtures (2026-10-03).** `F-DYN-SPAWN`, `F-DYN-JOIN`, and `F-DYN-CANCEL` are executable rules.
Vectors cover handle publication before any child transition, scheduler-capacity failure that drops
the callable's captures in the parent, child exception and trap propagation after child cleanup,
grant attenuation by host child policy, a child effect resumed through the parent's join,
cancelling an unstarted child, and dropping an unjoined handle to the reaper. Because interleaving
is not fixed by the language, the vectors use one stated schedule (the child runs when joined).
Grant revocation between two dispatches is vectored for the root and for a child. Still missing:
cancelling a child that is parked mid-run. `join-all`, `race-and-reap`, and `select-complete` have no rule or vector.

## Fourth-pass findings from executing the artifacts

### 57. The token-precedence lexer model could not lex the language — RESOLVED

**Finding.** `notation.json` said the token with the greatest precedence wins and length only breaks
ties, with every quoted punctuation terminal at precedence 200. Read literally, `:` always beat the
keyword token, `<` beat `<=`, `-` beat `negative_numeric`, and the JSON string and number tokens
beat every ordinary string and integer in CoLisp. No context-free lexer assignment of precedences
fixes this, because JSON islands and Co-Forth signature mode need context. The notation also never
said whether alternatives were ordered.

**Decision.** The notation is now `Finch-PEG-1`: an ordered-choice parsing expression grammar read
parser-directed. Terminals are matched only where a production asks, carry no precedence, and are
bounded by one word-boundary rule and one punctuation maximal-munch rule. A not-followed-by
predicate was added. Co-Forth "lexical modes" reduce to one rule: a parenthesized region is a
comment except where a production asks for `(`. Precedence numbers and four tokens no production
referenced were removed; the checker now rejects a dead token.

### 58. Ordered-choice defects in the published grammars — RESOLVED BY EXECUTION

**Finding.** Under any deterministic reading, these sources did not parse or parsed as the wrong
thing: a float (`1.5` read as integer then member access; `-3.25` as `-3`); `(let [mut x 1] ...)`
(`mut` read as the bound name); Co-Forth `Name{ ... }` (read as a word then a locals block) and
multi-word field values; `json[...]` (read as identifier then vector); `"""..."""` (read as an empty
string); `fiber[`, `syntax[`, `unsafe[`, `args{`, `rest{`; constructor patterns and tuple variant
cases (shadowed by the bare-identifier alternative); `=> target` operation bodies; `captures: move
{ ... }`; `as` import bindings; associated type values such as `Point<T>`; const arithmetic in a
generic argument (`array<T, n + 1>`); capability requests, which the prose spells
`namespace.operation{...}` and the grammar spelled with `::`; and `(get xs i)`, which was
unparseable because `get` is reserved and only the declaration form existed. `true`, `false`, and
`null` were bindable names. Co-Forth axioms and `ct-foreach` sources accepted only a single word.

**Repair.** Alternatives were reordered, the listed productions corrected, `index_form` added, member
declaration forms removed from the expression-level reserved forms, operator identifiers allowed as
operation names, and the three literal spellings reserved. Each defect has a corpus case or unit test.

### 59. Scope-guard semantics existed only as grammar productions — RESOLVED

**Finding.** `scope`, `defer`, `on-success`, `on-failure`, and `on-cancel` had productions and one
sentence about failure ordering. Nothing said which exits run which guard, whether `break`,
`return`, and tail calls count as success, what a guard body's type is, or whether a guard may
transfer out of itself. The rationale named three triggers; the grammar has four.

**Decision.** Section 8 now defines three exit classes and the guard table, LIFO order across guards
and drops, guard bodies of type `unit` or `never` as closed control contexts, the reason switch when
a guard fails on a successful exit, and uniform suppression under failure and cancellation.

**Owner decisions (2026-10-03).** The always-run guard is spelled `on-exit`, not `defer`, so the
four guards read as one family and `defer` is free as an ordinary prelude name. `on-failure` runs on
every non-success exit including cancellation, so compensation is written once. `on-cancel` stays
and runs in addition on cancellation: cancellation is not catchable, so without it code could not
tell a deliberate stop from a fault, and unlike a handler it cannot veto the unwind.

### 60. Co-Forth semantic construction was not specified — RESOLVED

**Finding.** The specification said both frontends build the same semantic program but never said
how a postfix word sequence becomes an expression tree. The previous harness hid this inside a toy
parser. Unstated: what a locals block binds and when, how `unit` appears on the stack, how a
discarded value is spelled, what a statement between two values means, and the ownership of an
unmarked stack entry. A Co-Forth local carried a mandatory type that CoLisp `let` could not spell,
so the "paired" fixtures only matched because the harness silently dropped the type.

**Decision.** Section 3.5 states the construction rules; CoLisp bindings take an optional type and
Co-Forth local types are optional, so the two always correspond. Every paired vector checks the
rules by building one AST from both spellings.

### 61. The rule programs were descriptive, and six forms had no rule — RESOLVED

**Finding.** `transitions.json` programs could not be run: branches named operands instead of
targets, loops had no back edge, `catch` and `guard` claimed to produce values, and the stated
invariant "every rule emits exactly one public transition" was false for assignment. `lambda`,
fiber construction, `yield`, `spawn`, `join`, and `cancel` had no rule at all.

**Repair.** See finding 35. The file also now states operand-temporary drop order, catch-clause
dispatch, and frame replacement on tail call, which no document previously fixed.

### 62. The semantic digest ignored structured attributes — RESOLVED

**Finding.** Event construction dropped every attribute whose value was an object or array. Two
programs that differed only in a constructor or record pattern had the same semantic digest.

**Repair.** Children are identified by an explicit list of child roles; every other field is an
attribute and is digested. A unit test proves binder, wildcard, and literal patterns hash apart.

### 63. Co-Forth cannot invoke a callable value — RESOLVED

**Finding.** CoLisp calls a closure held in a local with `(f x)`. In Co-Forth, naming a local pushes
its value, and the required prelude has no word that applies a callable on the stack. Three closure
vectors are therefore CoLisp-only.

**Owner decision (2026-10-03).** Functions and values share one namespace, so a callable behaves the
same whether `:` defined it or a local holds it: writing its name applies it. `' name` pushes the
callable without applying it, and `call` applies an unnamed callable on the stack. `call` is a
Co-Forth reader word, not a prelude operation, because it is the spelling of application itself.
This also closed a second hole: Co-Forth previously could not pass a `:` word as a value at all.
All closure vectors are now paired, and vectors cover `call` on a quotation and a ticked word given
to `spawn`. A local that holds a callable must declare its `callable<...>` type so its arity is known.

### 64. The replay automaton accepted an unsolicited resume — RESOLVED

**Finding.** A resume naming a new request identity, with the current generation and next sequence,
was accepted and dispatched even when the execution was parked on a different request. The prose
said the identity must be "bound to that sequence", but the automaton had no state to check it.

**Repair.** Automaton state gains `outstanding`; `F-REPLAY-UNSOLICITED` rejects a resume for any
other identity. Two replay fixtures and the hostile-resume vector cover it.

### 65. Lifetimes of borrowed arguments were unspecified — RESOLVED

**Finding.** Nothing said who owns a temporary passed to a borrowing parameter, whether a `Copy`
value passed to a readonly borrow is a loan, or what happens when a tail call passes a loan of a
local in the frame it discards. The last case is a dangling loan under proper tail calls.

**Decision.** `Copy` values pass by value to a readonly borrow; a borrowed temporary is owned by the
calling expression and dropped after the call; a tail call that passes a loan of a discarded
frame's place is rejected.

**Owner decision (2026-10-03).** A temporary passed to a borrowing parameter in a tail call is
adopted by the callee's frame rather than rejected.

### 66. Surface questions found by execution — OPEN, NON-BLOCKING

- Decided (owner, 2026-10-03): `value`, `state`, `stack`, `effect`, `arg`, `region`, `infer`, `plain`,
  and `some` stay reserved in both frontends. A corpus case pins `(let [value 1] value)` as rejected.
- CoLisp `(Name :key value ...)` is always a record construction, so a call that passes a keyword
  argument first cannot be written.
- Co-Forth has no tagged JSON literal; CoLisp has `json[...]` and `json{...}`.
- `get` is reserved in CoLisp for the getter declaration and, since finding 58, the index form.
- Co-Forth cannot assign or yield a `never`-typed expression (`5 throw to x`); the paired vectors
  use a throwing function instead.

### 67. A callable type's contract arguments cannot be written in source — RESOLVED

**Finding.** The required prelude spells callable types with five arguments, for example
`callable<unit,T,e,X,s>`: arguments, result, effect row, exception set, and suspension. The shared
type grammar accepts only types, constants, associated bindings, and refined paths as generic
arguments, so `effects<>`, `nothrow`, and `non-suspending` cannot appear there, and the prelude's
own signature parser accepts a spelling the language grammar rejects. The vectors use the
two-argument form `callable<int, int>`, whose meaning (remaining axes inferred) is not stated
anywhere, and the spelling of a multi-argument callable is not stated either.

**Decision (2026-10-03).** A callable type has one source spelling, the arrow form
`callable<(parameters) -> result ! contract>`. It reuses the declaration contract syntax, so all
three axes and every ownership mode are expressible with no new row notation, and several
parameters need no tuple convention. `callable` is reserved and the application spelling
`callable<int, int>` is a reader error. The five-argument constructor in the prelude is documented
as the kind-level view of the same type. The prelude signature parser still reads its own notation;
making it share the type grammar belongs with finding 47.

### 68. Runtime areas not yet specified to vector level — OPEN, ORDERED BY REWORK RISK

The owner's stated priority is to avoid reworking the interpreter. This lists what the vectors do
not yet pin, most architecture-shaping first, so the interpreter can be shaped for it up front.

1. **Scheduling beyond join-driven children.** The vectors run a child only when it is joined.
   Cancelling a child parked mid-run, `join-all`, `race-and-reap`, `select-complete`, fairness and
   tie-breaks, and streams (`next`) need a scheduler the host script can drive. The interpreter must
   already treat every execution as a separately resumable context with its own cleanup stack; that
   much is fixed by the fiber and task vectors.
2. **Checkpoint, restart, and durable replay.** The prose requires persisted generation, sequence,
   accepted requests, and terminal state, and the replay automaton covers message acceptance, but no
   vector restores an execution and resumes it. Frame, cleanup-stack, and journal representations
   must be serializable from the start.
3. **Foreign calls and callbacks.** Attachment, revocation, threading, and reentrancy (finding 52).
4. **Shared, Weak, and pinned owners.** `retain`, `weaken`, `upgrade`, and their drop order have
   prose and prelude signatures only.
5. **Data operations.** Field assignment (`set!` on a projection, `.name!`), `record-set!`, indexing,
   tuples and sequence literals, record-payload variant cases, strings and numerics beyond `int`.
   These add instructions but should not change interpreter structure.
6. **Compile-time forms.** `mixin`, `ct-foreach`, syntax quotation, and tests run in the staging
   evaluator, which the specification says is the same machine under a compile-time policy.

Variant construction and member reads were in group 5 and were closed on 2026-10-03.

### 69. Reflection record shapes are not normative — OPEN

**Finding.** `function-spec-of`, `members-of`, `fields-of`, and `modules-of` are required
compile-time operations, but `FunctionSpec`, `MemberSpec`, `FieldSpec`, and `ModuleSpec` are opaque
type names in the prelude with no fields. The rationale (`FINCH_LANGUAGE_DESIGN.md`, "Added
2026-09-18: `FunctionSpec`") says a `FunctionSpec` is a `ParameterSpec` plus `body : syntax`, which
is the hook a transformation needs to inspect a function's body; the specification never states it.
Two implementations could expose different reflection data, and a frontend without quasi-quotation
depends entirely on these records.

**Needed.** Record declarations for the four types in the required prelude, and one sentence on
dependency tracking: reading a function's `body` makes the client depend on that function's
CTFE-body identity, not only on its contract.

### 70. A third syntax as a test of the language — IMPLEMENTED FOR THE EXECUTABLE CORE

**Observation.** Pairing two syntaxes is what exposed findings 60, 63, and 67. A third syntax that
shares no accident with either is a cheap further test: it costs a grammar, an elaborator, and a
third spelling on the vectors, and changes nothing after semantic construction. The owner's working
notes for a D-flavoured C-like surface (camelCase names with a raw-identifier escape, `name!(...)`
type arguments, trailing contract attributes, syntax templates plus reflection hooks plus `mixin`)
are in `FINCH_LANGUAGE_DESIGN.md` under "A third, C-like syntax as a wart detector".

**Result (2026-10-03).** The owner asked for every feature to have a C-like, Co-Forth, and CoLisp
example. `grammar/clike.json` and its elaborator cover the executable core, and every execution
vector and static rejection now carries a C-like spelling that builds the identical AST and digest.
Nothing after semantic construction changed. The third reader found no new disagreement in the
vectors, which is evidence the core nodes are syntax-neutral; it did need the word-character class
to become per-frontend (`word_extra`), since `-` cannot continue a word where it is an operator.
Not covered in the C-like syntax: modules and imports, records as declarations, concepts and
implementations, tests, staging forms, floats, and the `&&`, `||`, `%`, and bitwise operators.

**What it asks of the specification.** Finding 69 (reflection records), and structured type nodes
in the AST in place of canonical type text: the C-like reader currently has to print the shared
type spelling.

### 71. The compile-time job scheduler is named but not specified — SCHEDULER RESOLVED, TWO QUESTIONS OPEN

**Finding.** Section 2 gives the monotonic symbol states (`Declared < SignatureReady < BodyTyped <
Lowered < FunctionCertified`) and says `require(identity, state)` returns, suspends the current
compiler job, or reports a deterministic cycle. That is the mechanism by which type checking, generic
instantiation, lowering, verification, and CTFE interleave: checking one declaration may need
another lowered, verified, and executed first. The specification does not say which operation
requires which state of which symbol (calling a function at compile time, reading its
`FunctionSpec`, instantiating a generic, resolving evidence), whether a compile-time call needs its
callees certified eagerly or on demand, whether CTFE of a generic runs its shared parametric IR or a
specialization, what the memoization key of an instantiation or a CTFE result is, or which
diagnostic is primary when a cycle is found. "Scheduling order MUST NOT change the sealed interface,
verified IR, or primary diagnostic" cannot be tested without those.

**Why it matters for the interpreter.** CTFE runs on the same machine as runtime code, so the
interpreter must execute individually certified functions from a module that is not yet sealed,
under a compile-time policy, and exchange `syntax` and `type` values with the compiler. If that is
decided late, the interpreter's module-loading and value model change.

**Ordinary values.** CTFE also returns plain values: `fib(10)` evaluated at compile time becomes the
constant `55` in the caller's IR. Two things about that are unstated. First, which positions force
compile-time evaluation of an ordinary call (a value-generic argument, a `where` constraint, an
associated constant, and a `ct-foreach` source clearly do) and whether source can force it anywhere
else, as D does with `enum`. Second, which result types may be embedded as constants: scalars,
strings, and aggregates of them can, while closures with captures, loans, fibers, tasks, and
resources cannot, and no judgment says so. The caller also takes a dependency on the callee's
CTFE-body identity, which section 2 names but no rule applies.

**Done (2026-10-03).** `semantics/compile-scheduler.json` gives the requirement table (seven kinds,
each with the state it requires and the dependency class it records), the progression and failure
rules, the cycle rule, and the identities of compile-time results and instantiations. The outcome is
a fixed point: each unmet goal waits on exactly one other, so cycles are well defined and reported
from their smallest goal. `scripts/language/compile_scheduler.py` runs it with a real ready queue,
and `fixtures/compile-scheduler.json` holds 28 cases that must give one outcome under 18 scheduling
orders. Decided in the model: compile-time evaluation certifies callees on demand, so an unexecuted
callee is neither required nor a dependency; a body is readable as syntax once declared but creates
a body dependency; mutual recursion and module import cycles are not symbol cycles.

**Still open (owner).** The list of positions that force compile-time evaluation and whether source
can force it elsewhere, and the judgment for values that may cross from compile time into a program
as constants. The model treats "evaluates" as given; it does not decide where one arises.

### 72. The implemented IR and the specified semantics have not been reconciled — RESOLVED FOR THE EXECUTABLE CORE

**Reading (2026-10-03).** `crates/finch-vm-core/src/ir.rs` (IR version 5) is a basic-block stack IR
with 34 instructions, verified by abstract interpretation of stack types
(`crates/finch-vm-core/src/verifier.rs`) and run by a trampoline over serializable frames
(`VmFrame`, `VmContinuation`, `VmStep` in `crates/finch-vm/src/interpreter.rs`). Both frontends
compile straight to it through `SemanticBuilder`; there is no shared AST, checker, or HIR between
reader and IR.

**What already matches the specification.** Serializable frames and operand stack; a step protocol
with `Emit`, `Await`, `Complete`, and `Failed`; a per-run effect sequence; typed stack signatures
carrying effects and suspension; closures with captures; fibers; fuel; per-function certification.

**What the specification requires and IR version 5 cannot express.**

1. Typed exceptions: no raise, handler, or rethrow. Only `PropagateResult` and `Trap` exist.
2. Cleanup: no guards, no drop obligations, no unwinding. `Trap` fails the run immediately, and a
   frame has no cleanup stack.
3. Ownership: values are immutable trees copied freely; there are no moves, loans, exclusive
   borrows, or `Unique`/`Shared` owners, and `RecordSet` builds a new record.
4. Proper tail calls: `Call` always pushes a frame.
5. Protected edges: `Failed` does not distinguish trap, cancellation, denial, and resource
   exhaustion, and none of them runs cleanup.
6. Types: records and variants are structural (specified nominal); `option` and `result` are
   built in (specified as library variants); function, task, fiber, and stream types carry no
   exception set, fibers no resume type, parameters no ownership mode; generics are unification
   variables with no evidence.
7. Fibers and tasks: `NextFiber` does not consume the handle and return a successor, resumption is
   one-way, and CPU fibers are pure only.

**Assessment.** Items 1, 2, 4, 5, and 7 are additive: new instructions, handler and cleanup
metadata on a function, and two more vectors on a frame, with the trampoline unchanged in shape.
Item 3 is the one that can force rework, because it changes how every instruction treats a value.
Item 6 and the missing middle of the compiler (finding 50, finding 71) change what is lowered, not
how it runs.

**Done (2026-10-03).** `semantics/ir.json` defines IR version 6 as version 5 plus region tables,
explicit drops, slot references, and tail calls: 38 instructions, each with fields, stack effect, and
the version that introduced it. `scripts/language/ir_lower.py` lowers the statically checked
program, `ir_verify.py` verifies structure and operand heights, and `ir_machine.py` executes with an
explicit operand stack and frame vector in the shape of the implemented interpreter. All 100
vectors lower, verify, and reproduce the rule machine's observable sequence, terminal, journal,
drops, host log, and peak frame count; every instruction is produced by at least one vector; each
vector pins its IR digest. The toy fixture IR and its schema are deleted.

Owner decisions applied: version 6 extends version 5 rather than replacing it, and ownership is
proven at compile time and erased. The executor keeps no ownership state. The run-time additions
to a frame are three lists that stay empty unless used (unwinding signals, active exceptions,
adopted tail-call temporaries).

**Not done.** The verifier checks structure and heights, not operand types, effect containment, or
exception sets. Items 3, 6, and 7 of the list above are specified for the core only: `Shared` owners,
nominal type declarations, generics with evidence, and the scheduler-facing task instructions beyond
join-driven execution are not in the instruction set. The existing Rust interpreter has not been
changed; this is the specification it would be extended to.

### 73. A browser target was raised early; what it would need — NOTED

**Question (owner, 2026-10-03).** Could a pseudo-module expose browser objects (window, DOM), with a
backend that emits JavaScript, and WebAssembly where appropriate, so UI code in the style of
Mithril can be written in Finch.

**How it fits.** Browser objects are host capabilities, which the language already routes through
`extern` declarations and typed effects. The pseudo-module is an extern module with a JavaScript
ABI whose values are opaque resource handles; its declarations can be generated from WebIDL. The
effect row of a function already says whether it touches the DOM, so the compiler can tell which
code must be JavaScript-facing and which is pure enough for WebAssembly. Erased ownership suits
both: a JavaScript backend leans on the collector and emits lifecycle drops as calls, and a
WebAssembly backend can use linear memory with the drops the compiler already placed.

**What must be decided before an interpreter or backend is built around it.**

1. Callbacks. The DOM is driven by handlers the host calls later and re-entrantly. That is the
   unspecified callback attachment, revocation, and reentrancy contract of finding 52, and it is a
   prerequisite here.
2. Integer width. `int` is `i64` with overflow traps. JavaScript has no fast checked 64-bit
   integer, so a JavaScript backend pays for `BigInt` or the language needs a stated narrower
   default for that target.
3. Suspension. Only functions whose contract says `suspends` need to become generators or async
   functions; the static suspension axis already identifies them. A JavaScript generator cannot be
   serialized, so checkpointing and durable replay would not be available on that backend and must
   be a profile the target does not claim.
4. Tail calls. JavaScript engines do not guarantee them, so the backend needs loops or trampolines
   to honour the proper-tail-call rule.

**TypeScript as a frontend (owner question, 2026-10-03).** Real TypeScript is not a candidate: it
assumes a garbage collector and unrestricted aliasing, its type system is deliberately unsound and
structural, it is built on untagged unions, `null`/`undefined`, and one floating `number` type, and
its object model is prototypes with dynamic property access. A TypeScript-shaped *dialect* is
possible, as AssemblyScript shows, but it would not compile existing TypeScript, and the C-like
syntax already fills that role; its spellings could lean toward TypeScript where they do not
conflict. The part worth taking is the `.d.ts` format: it is the de facto interface description for
JavaScript libraries and the DOM, so it is the natural source from which to generate the extern
modules a JavaScript target needs.

**Both directions of `.d.ts`, and build order (owner discussion, 2026-10-03).** *Import:* a tool
reads `.d.ts` files and generates the extern module a Finch program calls. *Export:* compiling a
module for a JavaScript target also emits a `.d.ts` describing its exports, derived from the sealed
module interface, so TypeScript and JavaScript projects can consume it with types. Neither needs a
TypeScript source frontend. Both are lossy: TypeScript cannot express ownership, effects, or exact
integer widths, and untagged unions and `any` have no direct Finch type, so import needs stated
rules (for example, every JavaScript object is an opaque shared handle).

A consequence: a project's own TypeScript modules must be compiled before a Finch program can call
them, because Finch reads their declarations and links their JavaScript, not their source. Published
packages already ship both. The reverse dependency compiles Finch first. A cycle across the language
boundary needs declarations produced without the other side, which each compiler can do (Finch from
its sealed interface, TypeScript under `isolatedDeclarations`); the simple rule is to forbid such
cycles.

**Where the JavaScript/WebAssembly cut falls (owner, 2026-10-03).** The owner prefers not to cut at
module boundaries. The cut can be inferred per function from its effect row: a function whose row
contains a capability provided by a JavaScript extern is JavaScript-side, and every other function
may be WebAssembly. Effect rows already include callees' effects, so no extra analysis is needed,
and it gives a useful property: WebAssembly-side code cannot call JavaScript except through a
closure it was handed, so ordinary crossings run one way. What this needs:

1. A representation for values that cross the cut. Scalars and opaque handles are free; records,
   strings, and closures must be marshalled or kept in WebAssembly memory with JavaScript views. The
   compiler either generates that or rejects types with no defined crossing.
2. A function that is polymorphic in its effect row is compiled for each side it is instantiated
   on, which the shared-body-plus-specialization rule for generics already allows.
3. A way to see and override the decision, since an inferred cut has an invisible cost: a report of
   which side each function landed on and what crosses, and an attribute to pin a function.

`int` narrowing would then apply only to JavaScript-side code, since WebAssembly has native 64-bit
integers.

### 74. Target constraints as compiler input — DECIDED IN OUTLINE, DETAILS OPEN

**Decision (owner, 2026-10-03).** The default integer is narrower on a JavaScript target rather
than paying for 64-bit emulation, and the target's constraints flow through the compiler so that
integer coercions that do not hold on the target are compile errors. The same machinery serves
cross compilation, and a compile-time hook lets a library specialize a block of code per CPU type.

**How it fits what exists.** A target already has a canonical identity tuple that includes
`pointer-width` and a `scalar-layout-table` (section 16, `schemas/target-registry.json`). Today
`int`, `uint`, and `float` are fixed aliases of `i64`, `u64`, and `f64`. The change is to make
those three aliases entries of the target's scalar layout table, keep `i8` through `u64`, `f32`,
and `f64` exact everywhere, and expose the target identity as a compile-time value.

**Consequences that need stating before this is specified.**

1. Compile-time evaluation runs on the host but must compute with the target's arithmetic. The
   interpreter therefore takes integer widths and trap rules as parameters; it cannot assume its
   own. This applies to the reference machine's `int` bounds as well.
2. A function that reads the target depends on it. Target identity joins the memoization key of
   compile-time results and instantiations (finding 71) and the dependency fingerprint.
3. `int` in a published or portable interface would make that interface mean different things on
   different targets. The natural rule is the one C and Rust converged on: target-sized integers
   are for local arithmetic and indexing, and anything crossing a portable message, a
   `repr(stable)` record, a checkpoint, or a cross-target module boundary uses an exact width.
4. A literal or conversion that fits one target's `int` and not another's is an error on the
   narrower target, reported with both the value and the target.
5. Per-target specialization needs a conditional *declaration*, not only a conditional expression:
   a way for compile-time code to choose which definition exists. `mixin` of generated syntax can
   already do it; a direct form in the manner of D's `static if` or `version` would read better.

**Owner requirement (2026-10-03, second note).** Defaults such as `int`, `uint`, and pointer widths
*should* change with the target, but moving a program from a 64-bit to a 32-bit target must not
silently produce code that misbehaves. Anything that becomes a potential bug under the narrower
width has to be a compile error.

**Proposed rule set that meets it.**

1. `int` and `uint` are distinct target-sized types, not aliases. Their width is the target's, with
   a stated minimum of 32 bits and maximum of 64.
2. An implicit conversion is allowed only when it is lossless on every admissible target:
   `i32 -> int` and `int -> i64` are; `i64 -> int` never is and needs an explicit checked or
   truncating operation, on a 64-bit target as much as on a 32-bit one. The mistake is therefore
   rejected where it is written, not discovered when the target changes.
3. A literal or compile-time-evaluated value that does not fit the target's `int` is a compile error
   naming the value and the target.
4. A package declares the targets it supports. Checking is done against all of them, so a build for
   a 64-bit host still reports what would break on a declared 32-bit target.
5. What is left is run-time arithmetic that exceeds the narrower range. Integers trap on overflow in
   every profile, so that case stops with a diagnostic; it never continues with a wrapped value.
6. Quantities whose range is set by the problem rather than by memory (file offsets, timestamps,
   identifiers, money) are declared with exact widths in library signatures; `int` is for counts
   and indices bounded by the address space.

**No separate `static if` (owner, 2026-10-03).** An ordinary `if` over a compile-time target query
should be enough. `static if` differs from `if` in exactly two ways, and each has an answer without
a new form.

- *It can choose declarations, not only values.* A module body has no expressions, so an `if`
  cannot sit there. `mixin` already covers this: compile-time code runs an ordinary `if` and returns
  the syntax of the declaration to emit. The branch not taken is quoted syntax and is never analysed.
- *Its untaken branch is not compiled.* For an ordinary `if` both arms are type-checked, which is
  an advantage (code for other targets cannot rot unnoticed), but only the taken arm may reach
  code generation, or a reference to a symbol that does not exist on this target fails to link.
  That needs one guarantee rather than a new form: when the condition is a constant expression,
  only the selected arm is lowered. It in turn needs "constant expression" defined (literals,
  `comptime` queries, and pure calls over them) and folded as a guarantee, not as an optimization.

A branch that cannot even type-check on the current target (it names a type or field that does not
exist there) is the remaining case, and it goes through `mixin`.

**Checked per target, not skipped.** D's `static if` only parses its untaken branch; nothing in it
is resolved or type-checked, so a branch for another platform can rot until someone builds there.
The owner is not convinced that is desirable. Because checking already runs against every declared
target (rule 4 above), Finch can do better than either skipping or requiring every arm to check
everywhere: each arm is checked under the targets that select it. An arm selected by no declared
target is reported as unreachable. The same holds for `mixin`, whose expansion is computed per
target. Only code that reads the target is rechecked per target; the dependency edges of finding 71
identify it.

**Cost of per-target checking (owner concern, 2026-10-03).** Checking each branch under its target
sounds like type-checking the program once per target with different integer types. It need not be,
and the distinct-type rule is what prevents it. If `int` is an opaque type whose conversions are
legal only when lossless on every target, then no typing judgment depends on its width: the type
checker runs once, with `int` abstract. The target is in effect one implicit generic parameter of
the program, handled the way the specification already handles generics: one verified parametric
body, specialized at lowering. What is genuinely per target is small and separable:

- range checks on literals and compile-time-evaluated values;
- which arm a constant target condition selects, and so which arm is lowered;
- layout and size queries, and `repr(C)`;
- `mixin` expansions that read the target, and whatever uses the declarations they produce.

Only the last item re-runs type checking, and only for the code that depends on it. If `float`
stays `f64` everywhere, it adds nothing to this list.

**The case the single-check rule gets wrong (owner, 2026-10-03).** Inside an arm that only 64-bit
targets select, `i64 -> int` is lossless, yet the rule above still rejects it as an implicit
conversion. Two ways to resolve it:

- *Keep typing target-independent.* The conversion is written explicitly even there. In an arm
  where the target makes it lossless, the range check is provably dead and costs nothing. The type
  checker never evaluates a target condition. The cost is one visible conversion per use.
- *Refine by branch.* Within an arm guarded by a target condition, the checker knows which targets
  remain and allows conversions that are lossless for all of them. This is more convenient and is
  what D's `static if` gives, but typing then depends on evaluating target conditions, which is
  the per-branch specialization the owner was wary of.

The reviewer leaned to the first.

**Owner direction (2026-10-03): the architecture should be typeable.** Rather than branch on a
target name, code asks a question about types, in the manner of D's `is(foo : int64)`, and the arm
that asked may rely on the answer. That is a third resolution and it is better than both above.

- A lossless-conversion fact is intrinsic evidence, like the existing `ExceptionSubset<From,To>`:
  say `Widens<From, To>`. A target supplies `Widens<uint, i64>` when its `uint` is 32 bits and does
  not when it is 64. The target is described by which evidence it provides, not by its name.
- A compile-time test asks whether evidence exists. The arm it guards is type-checked under that
  evidence as a hypothesis, exactly as a generic body is checked under its bounds. Inside it the
  implicit conversion is legal; outside it is not.
- Because an arm is checked under its hypothesis rather than under a real target, it is checked
  once, on every build, whichever target is being built. Nothing is skipped and nothing is
  re-checked per target. Per target the compiler only decides which hypotheses hold, and so which
  arm is lowered. An arm whose hypothesis holds on no declared target is reported as unreachable.

This uses the evidence environment the type checker must have anyway, instead of a second mechanism
that evaluates target conditions during typing.

**Decided (owner, 2026-10-03): type machinery only.** A test on a value, such as comparing a
target's integer width to 64, never changes what the type checker accepts. Refinement comes only
from type-level facts: evidence tests and matches on a type. A compile-time target value may still
exist for choices that do not affect typing, such as picking an algorithm, and an ordinary `if` over
it behaves like any other `if`. This withdraws the two value-based options listed earlier in this
finding.

**Matching on a type (owner, 2026-10-03).** The same hypothesis rule gives a `match` whose scrutinee
is a compile-time type: each arm is checked once under the assumption that the type is that arm's
pattern. It improves on a chain of tests because it can be exhaustive. `int` is one of a closed set
of widths, so a match over it that omits a width some declared target uses is a compile error, and
an arm no declared target selects is reported as unreachable.

**Scope.** D's `static if` does not introduce a scope: a declaration in its body is visible after
it. Finch's `if` does introduce one, and that should stay. The uses of the D behaviour have other
spellings here: a value chosen per arm is the value of the `if` expression; a type chosen per arm is
an associated type of the evidence; a declaration chosen per arm is produced by `mixin`, which
places it in the enclosing module.

The cost of a scoped `if` is that code following a per-arm declaration cannot simply continue after
the block. The owner's answer (2026-10-03): put the shared code in a local generic function and
call it from each arm, or expand a syntax template in each arm. They differ in when the shared code
is checked. A generic function is checked once against its bounds, with no caller involved, and the
compiler may inline it. A template expansion is checked again in every arm it is expanded into; it
can use an arm's local names without passing them, and its errors point into generated code. The
function is the default; the template is the explicit escape. When the arms can first convert to a
common type, neither is needed: the `if` yields that value and the shared code follows it.

### 75. Open questions, in one place

Questions for the owner. Each names what it blocks.

*Language surface*

1. Confirm the proposed rule set for target-sized integers: `int` distinct from the exact widths,
   implicit conversion only when lossless on every target, checking against every declared target.
   On a JavaScript target `int` would be 32 bits. (finding 74)
2. May `int` appear in a published interface that crosses targets, or only exact widths there?
   (finding 74)
   Decided: target differences are visible to typing only through type machinery (evidence tests
   and matches on a type), never through a value comparison. Still to choose: the spelling of the
   evidence test and of the type match in each syntax. (finding 74)
3. No `static if`: per-target code uses ordinary `if` over a compile-time target query, with a
   guarantee that a constant condition lowers only its selected arm, and `mixin` for declarations.
   Confirm, and name the target query. (finding 74)
4. Is there a way to force compile-time evaluation of an ordinary expression, or only the positions
   that already need a constant? (finding 71)
5. Which values may cross from compile time into a program as constants? Proposed: the same
   judgment as checkpointable data. (finding 71)
6. CoLisp `(Name :key value)` is always a record construction, so a call whose first argument is a
   keyword cannot be written. Keep, or give calls a distinct spelling? (finding 66)
7. Should Co-Forth have a tagged JSON literal like CoLisp's `json[...]`? (finding 66)
8. Co-Forth cannot use a `never`-typed expression as an operand (`5 throw to x`). Accept as a
   property of postfix code, or add a spelling? (finding 66)
9. The C-like syntax has no `&&`, `||`, `%`, or bitwise operators, and no modules, records as
   declarations, concepts, or staging forms. Which of these are wanted, and with which spellings?
   (finding 70)
10. Does the C-like syntax get a name? The artifacts call it `clike`.
11. Should cancellation be deferrable across a critical section (a shield), given it is not
    catchable? (finding 59)

32. Decided (owner, 2026-10-03): dropping the handle of an unfinished fiber or task cancels it at
    the drop; its cleanup runs there, with exit class cancel, as part of this execution's sequence.
    Not yet implemented in the reference machines or pinned by vectors. (finding 78)
33. Fiber resume values: the resume binding owns each step input and `yield` reads it (chosen), or
    `yield` returns the input owned and the binding holds only the first. (finding 78)
34. Decided (owner, 2026-10-03): a local type has no hidden context and no capture list. Whatever
    it needs from the enclosing function is passed through its constructor and stored in fields.
    Still open: whether local function declarations are wanted, and whether they are capture-free
    functions or named closures. (finding 80)

*Repairs made without asking, to confirm*

11a. Redeclaring a function in a later turn shadows it; earlier functions keep the revision they
    were checked against. (finding 76)
11b. A capability request whose arguments are not static needs an unrestricted grant at admission.
    (finding 76)
12. The grammar notation is an ordered-choice, parser-directed grammar instead of a precedence
    lexer. (finding 57)
13. Tail position propagates through `let` and `scope` bodies. (section 12)
14. Task vectors use one fixed schedule: a child runs when it is joined. (finding 56)
15. A function named in value position is a capture-free callable, and field types of an
    undeclared record are learned from its construction sites. (reference harness)

35. Handler arms and the body of `try` are not tail position, and `return` inside them is not a
    tail call. (finding 78)
36. A tail call may pass on a loan its frame received; an adopted value follows that loan.
    (finding 78)
37. A contract that states one axis must state the exception and suspension axes, enforced in
    every frontend; the C-like attribute words are reserved. (finding 78)
38. `names` in the compile scheduler requires the named symbol's header, not only its existence.
    (finding 78)

*Runtime and backends*

16. The callback contract: how a closure is handed to a host, called back re-entrantly, and
    released. It blocks the browser target and C callbacks. (findings 52, 73)
17. Is calling C functions wanted in 0.1, with embedding Finch in other hosts explicitly out of
    scope? The owner indicated yes; the specification's FFI profile still describes both.
18. For a browser target: is checkpoint and durable replay simply not offered there, and are tail
    calls honoured by trampolining? (finding 73)
19. Exceptions and cleanup in the IR are per-function region tables, with drops placed by the
    compiler and no run-time ownership state. Built and checked against every vector; confirm the
    shape before the Rust interpreter is extended to it. (finding 72)
20. A tail call that hands a temporary to a borrowing parameter needs the callee frame to own it.
    That is one small run-time list per frame, empty unless used. Acceptable? (finding 65)

*Work that needs no decision but is not done*

0. What a session does with a turn that is parked on a request when the next submission arrives.
   Manifest, admission, turns, compile-time authority, and the direct binding are done.
   (finding 76)
21. IR version 6 remainder: operand-type, effect, and exception checks in the verifier; the
    shadow region for a `try` body that moves an outer binding. (finding 72)
23. A host-drivable scheduler: cancel mid-run, `join-all`, `race-and-reap`, `select-complete`,
    streams. (findings 56, 68)
24. Checkpoint and restart vectors. (finding 68)
25. Reflection records: `FunctionSpec`, `MemberSpec`, `FieldSpec`, `ModuleSpec`. (finding 69)
26. Structured type nodes in the AST instead of canonical type text. (finding 70)
27. The type checker proper: inference, generics, evidence, effect rows, exception sets.
    (finding 50)
28. `Shared`/`Weak` lifecycle, field assignment, indexing, tuples, floats. (finding 68)
29. An independent consumer for the prelude signature AST. (finding 47)
30. Stable payload identity binding. (finding 53)
31. Measured performance evidence, which needs an implementation. (findings 48, 54)

### 76. The primary use (an LLM-driven REPL) and compiled targets need different effect bindings — OPEN

**Owner statements (2026-10-03).** The language's original purpose is to be embedded in a REPL that
an LLM drives across turns. There it must be pure in this sense: the equivalent of `printf` sends
an event to the hosting program, which decides what to do with it; the host states which
capabilities are allowed or denied, including during compilation; and the capabilities a checked
program needs are known before it runs, so a user can be prompted. In native or JavaScript output
none of that mediation is wanted, because it would mean embedding a slow runtime.

**What the specification already has.** Effects are typed requests (`Emit`, `Await`) that are
journaled before exposure; a submission's inferred entry contract is checked against the submission
policy; `inferred request <= declared request <= active grant <= host policy`; the host rechecks the
live grant before each dispatch; only `Complete` commits VM-local mutation.

**What is missing.**

1. *The manifest as an output.* The set of capabilities a checked program may request, with their
   selectors, is implied by its entry effect row but is not a defined artifact handed to the host
   before execution, and there is no admission outcome for "not granted, never started". Every
   current vector starts the program and lets a missing grant surface at the first dispatch, after
   earlier effects have already happened. Both layers are needed: an upfront check for the prompt,
   and the dispatch check for revocation and for selectors that depend on run-time values.
2. *Turns.* A session keeps definitions across submissions and a failed submission commits nothing.
   The rule is stated; no vector runs two submissions against one session.
3. *Compile-time authority.* `include-str` and `include-bytes` read files during compilation, and
   compile-time evaluation is otherwise forbidden host effects. The grant set that governs
   compilation, and how it is requested, is not specified.
4. *Two bindings of an effect.* The same checked program must run in two ways:
   - **mediated**: each effect is a request to a host broker, with journal, replay, live grants,
     and suspension. This is the REPL, and it is what the vectors assert.
   - **direct**: each effect is linked to a provider and called like any function, with no journal,
     no replay, no mid-run revocation, and no runtime beyond the compiled code. Grants are checked
     once, at build or admission, against the manifest. This is native and JavaScript output.
   The static checks and the order of effects are the same in both. Section 2 says journaling is
   paid for only when used, but nothing names the two bindings, says which guarantees each drops,
   or defines conformance for the direct one. This axis is separate from hosted versus unhosted
   admission, which is about unsafe code.

**Why it matters now.** The IR has one `capability_request` instruction. If binding is a property of
how that instruction is realized (a broker round trip, or a direct call), the interpreter and the
compiled back ends share everything else. That should be stated before either is built.

**A longer goal that tests the split (owner, 2026-10-03).** Embed LLVM, boot a machine directly into
the REPL and compiler, and compile the operating system's source on the fly. That system uses both
bindings at once: the kernel, drivers, and compiler are compiled natively with direct binding, and a
submission typed at the REPL runs mediated on top of them. It also shows that binding is independent
of the execution engine: a mediated submission may still be compiled to native code and run in
process, with only its effects going through the broker. So there are three separate axes, and the
specification should name each: admission (hosted or unhosted, about unsafe code), effect binding
(mediated or direct), and engine (interpreter, just-in-time, or ahead-of-time). What it asks of the
specification beyond this finding: a freestanding target in the target registry, device access
expressed as capabilities granted by the boot environment, and the unsafe subset (raw pointers,
`repr(C)`, calls to C and assembly) complete enough to write drivers. Durable replay of suspended
native frames is the hard part and may remain an interpreter-only profile.

**Item 1 done (2026-10-03).** `semantics/admission.json` defines the manifest and the admission
step, `scripts/language/admission.py` is the reference, and specification section 14.1 states them.
Every execution vector pins its manifest, which is derived twice (from the resolved program and
from its IR) and must agree. Thirteen vectors cover lazy versus preflight on the same program,
refusal with no transition at all, prompts answered allow and deny, requests on untaken branches
and in uncalled closures, an unreferenced function staying out of the manifest, grants limited to
particular arguments, and a revoked grant denied at dispatch after admission. One decision was
made in the model: a request whose arguments are not static is covered only by an unrestricted
grant. In the full language a refined argument type or selector bounds such a request statically;
the reference model has only exact arguments or none. Items 2, 3, and 4 remain.

**Items 2, 3, and 4 done (2026-10-03).**

- *Turns.* Specification 14.2 and `scripts/language/session.py`. `fixtures/session-vectors.json` has
  11 sessions in all three spellings: declarations persisting; a failed, trapped, rejected, or
  refused turn committing nothing; events of a failed turn standing; shadowing; a failed
  redefinition leaving the earlier revision; declarations in one turn seeing each other; a variant
  persisting and refusing redeclaration; a top-level binding not leaking. Each turn is also lowered
  to IR and compared. **Decision made without the owner, to confirm:** redeclaring a function
  shadows it for later turns and leaves earlier functions on the revision they were checked against.
  The owner was asked and had not answered; the alternatives were to replace everywhere or to
  reject redefinition.
- *A finding inside this work.* A Co-Forth turn cannot be read without the session: the reader needs
  the stack signature of every word, including those from earlier turns. An unknown word is now
  `F-DIAG-UNBOUND-NAME` at construction rather than a guess that it is a value.
- *Compile-time authority.* Specification 14.4 states the rule: only `include-str` and
  `include-bytes` reach the host during compilation, against a separate compile-time grant, with a
  denial as a compile error and each file read recorded as a dependency. It has no executable model,
  because the reference harness has no compile-time evaluation.
- *Direct binding.* Specification 14.3 names the two bindings and what each guarantees. The IR
  executor runs either. 112 of the 132 execution vectors do not depend on mediation, and each is
  re-run under the direct binding and must give the same effects in the same order, the same
  terminal, drops, and frame count, with no journal.

**Still open in this finding.** Parked turns (a turn suspended on a request when the next
submission arrives) have no rule. The three axes (admission, binding, engine) are named in 14.3,
but only the first two have artifacts.

**Needed originally.** A manifest artifact and admission step with vectors (granted, prompted, refused before
anything runs); session vectors across turns; a compile-time grant rule; and a definition of the
direct binding with the vectors re-run under it, comparing effect order, terminal, and drops.

### 77. Fixed arrays of fixed arrays have no stated layout, indexing, or equality — OPEN

**Finding (owner, 2026-10-03).** `array<T,N>` is a core type, and nothing says what an array of
arrays is. A multidimensional static array should be one contiguous block, and that choice changes
how indexing, bounds checks, and equality work, so it has to be specified rather than left to each
implementation.

**What needs stating.**

1. *Layout.* `array<array<T,C>,R>` is `R * C` elements of `T` in row-major order with no
   indirection and no per-row header. Nesting fixed arrays to any depth never introduces a pointer.
2. *Indexing.* A multi-index read computes one offset, `i * C + j`. Each index is checked against
   its own dimension. Checking only the flat offset would accept `a[0][C]` as the first element of
   the next row, which is a silent wrong answer rather than a trap.
3. *Bounds-check elimination.* The proof obligation is per dimension, and a loop over one dimension
   discharges only that dimension's check.
4. *Equality.* The shape is part of the type, so two arrays that compare have the same shape and
   nothing about shape is compared at run time; arrays of the same total size and different shape
   are different types and do not compare. Equality is element-wise under the element's `Equal`.
   It may be done as one block comparison only when the element's equality is exactly bitwise:
   not for floats, where a NaN is unequal to itself, and not for elements with padding.
5. *Views.* A row is a loan of a contiguous sub-block and satisfies `Contiguous`. A column is
   strided and does not.

**Owner direction (2026-10-03).** Keep `array<T,N>`. Multidimensional sugar lowers to a shaped type
`static-array<T, N, H, K, ...>` whose operations live in the standard library. The reviewer first
read this as "nested `array` types normalize to the shaped type"; the owner rejected that, rightly:
a rule that rewrites `array<array<T,C>,R>` is not expressible in the generic system without
recursive type-level definitions, and it would make `array<U,N>` behave differently depending on
what `U` is. That reading is withdrawn. No type normalizes into another.

**The sugar (owner, 2026-10-03).** A declaration such as `int[3][4][10] foo;` is reader sugar that
lowers directly to `static-array<int, ...>` with those dimensions. This is a frontend rewrite of one
syntactic form into one type, so it needs nothing from the generic system.

**Decided (owner, 2026-10-03): three bracket forms, each lowering one consistent way.**

| Written | Lowers to | Contiguous |
|---|---|---|
| `T[3, 4, 10]` | `static-array<T, 3, 4, 10>` | yes: one block, indexed `foo[i, j, k]` |
| `T[N]` | `array<T, N>` | its `N` elements |
| `T[]` | `vector<T>` | its elements, resizable |

The rule is that one bracket is one array and nesting brackets is composition. `int[3][4][10]`
lowers, like any other suffix, to `array<array<array<int,3>,4>,10>`: an array of ten elements, each
of which is an array of four, each of which is an array of three. It is an array of arrays and
makes no promise that the whole is one block; each level is indexed on its own. `int[][][]`
composes resizable arrays the same way and has no contiguous form. The only spelling that promises
a single contiguous block across dimensions is the comma list.

This supersedes the reviewer's earlier text in this finding, which asserted that nested fixed
arrays are stored inline and are therefore contiguous. The owner did not ask for that guarantee,
and it is not free here: generic code is one shared body by default, so an `array<T,N>` whose
element layout is visible to generic code needs layout passed in or specialization. Stated as the
language sees it: `static-array` provides `Contiguous` over its element type; a nested `array`
provides it over its own elements only, which are arrays. An implementation may still store nested
fixed arrays inline, since a program cannot observe the difference except through that evidence.

Unchanged from above: the shape of `static-array` is in index order, its operations are standard
library code over a value pack, equality is element-wise with a block-compare specialization chosen
by bitwise-equality evidence, and each index is checked against its own dimension.

**Still to choose.** Whether CoLisp and Co-Forth get a sugar or write the type names directly;
whether a zero-length dimension is allowed; the name of the bitwise-equality evidence. The three
bracket forms are now in `grammar/clike.json` as type suffixes and lower to the type names above.
`static-array` and `vector` have no prelude definition or operations yet, and there is no index
expression (`foo[i]`, `foo[i, j]`) in any frontend.

### 78. Hostile review round after the IR, admission, and scheduler work — FIXED, THREE DECISIONS OPEN

An independent review ran the reference pipeline against programs chosen to break it. Every claim
below was reproduced before anything was changed.

| What was wrong | What an implementer would have hit | Now |
|---|---|---|
| A value moved on one branch was dropped at scope end by the rule machine and at the join by the IR | two conforming implementations emit drops in different orders around later events | the static pass annotates each path; both drop as the path ends; six vectors |
| A `try` body that moved an outer binding could not be lowered | the IR had a reported gap | the drop obligation moves inside the catch region; vectors for raise before, raise after, trap, handler-only move |
| A callable that passes its borrowed parameter on in a tail call was legal or not depending on its caller | "a body means the same at every call site" was false; the IR handed the callee a dropped value | an adopted value follows the loan into the next frame (`forward` on `tail_call`); bounded by the callee's arity; three vectors |
| `return` inside a `try` body or fiber body was a tail call | the handler never saw the exception; the two machines disagreed on fibers | not tail position; two vectors |
| A handler arm was tail position | the caught value was dropped before the callee that borrowed it ran | a handler arm is not tail position; one vector |
| A non-`Copy` fiber resume value was owned twice | the rule machine dropped it twice and the IR leaked the previous one | the resume binding owns each input; `yield` reads it; three vectors |
| A closure in a tail call could outlive what it borrowed or be dropped before it ran | use after drop that both machines agreed on, or that only one rejected | a closure borrowing the discarded frame is rejected; adoption follows captured loans; a tail call through an owned closure moves it into the new frame; two vectors and a rejection |
| The manifest read from source and from IR differed for a wrapped literal | admission outcome depended on which one the host used | the request instruction carries a static claim the verifier checks |
| "Manifest equals the effect row" contradicted "includes closure bodies" | a host using the effect row admits a program the reference refuses | stated as a superset |
| `names` needed only that the other symbol exist | a function was certified against a type whose header failed | requires the header; `uses-members`, `embeds`, `reflects-members` added; eight cases |
| An inferred signature waited only for its callees | sealed before a body constant it depended on existed | issues every body requirement |
| A declaration inside a block reached the machine as an unknown form | crash-shaped error; C-like and Co-Forth disagreed on a declaration after a local | coded rejection; C-like lifts top-level declarations; two vectors |
| Contract axis rules were prose only | `! nothrow` alone was accepted | `F-DIAG-CONTRACT-AXES` in every frontend; two vectors |
| C-like comparisons were said to associate | the grammar rejects `a < b < c` | prose says non-associative |
| camelCase conversion depended on Unicode case tables | two implementations disagree on a name and so on linkage | defined on ASCII ranges only |
| C-like attribute words were not reserved | `int function(int) pure g` could not be parsed | reserved |
| Not-admitted was "no transition" yet pinned as one | contradiction | one `NotAdmitted` entry, stated; prompt order stated |
| The replay automaton as written rejects every resume | the reference silently ran two instances | two instances stated, one per event kind |
| `transitions.json` gave `branch` operands as if they were labels | rule programs could not be run from the text | evaluation order rewritten |
| `ir.json` was silent on adopted values at `return`, closure capture modes, intrinsic names, payload order | pinned only by traces | stated |

Open, for the owner:

1. **Dropping the handle of a fiber or task that has not finished.** The reference marks it for the
   reaper and runs none of its cleanup, so an owned value it holds is never dropped in the observable
   sequence (`cancel` of an unstarted task does drop its captures). Either the reaper's unwinding is
   part of this execution's sequence and runs at the drop, or it is a separate sequence the host
   must drain before the transaction commits. Nothing pins it today.
2. **The resume binding rule above is one of two coherent choices.** The other: `yield` returns the
   step input as an owned value and the resume binding holds only the first input. That one needs no
   special reading rule and lets a fiber consume every input; the chosen one keeps the existing
   text that the binding is initialized from each step. Under the chosen rule a fiber can consume
   an owned input only if no `yield` follows.
3. **A caught value has no static type.** The reference static pass types a catch binder as unknown
   and treats it as `Copy`; passing it to a borrowing parameter then disagrees with execution. The
   exception set of the `try` body gives the type; that is type-checker work (finding 50).

Still unstated in Co-Forth (section 3.5): an arm may not consume a value pending before its
construct; a locals block needs an otherwise empty stack; an uninitialized local assigned in both
arms of an `if` is rejected; a quotation's or fiber's declared result is not checked and is not in
the digest; a signature has exactly one result although section 3.3's test form shows a stack row;
an untyped local holding a quotation cannot be applied. These are restrictions of the reference
reader, not decisions.

### 79. Aggregate results are returned in caller storage — DECIDED

Owner decision: a result that is not an intrinsic scalar is promoted to the caller's frame and
passed by an invisible pointer, as D does. Section 16 states it: the caller reserves the slot, the
callee constructs in place, construction-then-return never copies or moves, a tail call forwards
the hidden address, and a `join` or `step` result slot belongs to the joining or stepping frame.

What it touches later: the native lowering of `return` and `tail_call`; the layout pass must know a
type's size before any caller is lowered (the `embeds` requirement in the scheduler already orders
that); a fiber or task that outlives the frame that will receive its result needs the slot chosen
at `step` or `join`, not at construction. The IR stays a value-stack model.

### 80. Local types that can be returned (Voldemort types) — WANTED, NOT YET IN THE CORE

Owner decision: a record or variant declared inside a function, returned from it, usable by the
caller, and nameable by nobody outside. Section 9 states the rule in outline. Finding 78 made a
declaration inside an expression a coded rejection in the executable core; that is the core's
present limit and is worded so in the specification.

What it needs:

- a declaration form allowed in a function body in each syntax (the C-like grammar already parses
  one in any block; CoLisp parses it; Co-Forth has no spelling);
- inferred result types for the function that returns one, which the scheduler handles as an
  inferred signature;
- a type identity made of the enclosing function, its generic arguments, and the local name;
- a way to bind without naming (`auto` in the C-like syntax, an untyped `let` and locals entry
  elsewhere) and a reflection query for "the result type of f";
- the sealed interface entry for an exported function that returns one.

Decided: a local type has no hidden context pointer and no capture list. Anything it needs from
the enclosing function's locals is passed to its constructor and held in fields, so a returned
value never refers to a dead frame and the ownership pass needs no new rule. It may use the
enclosing function's generic parameters and compile-time constants directly.

To decide: whether a local function declaration is wanted on the same footing; whether two
calls of a non-generic function return the same type (they would, by the identity above).

### 81. Fibers and tasks: generators as ranges, and who keeps the work alive — CORE IMPLEMENTED, SCHEDULED FORMS OPEN

Owner decisions (2026-10-03), reached while discussing finding 78's open points. They supersede
the resume-binding rule of finding 78 and decision 32 of finding 75 where they differ.

1. **Construction is `defer`, as the design document already had** (`FINCH_LANGUAGE_DESIGN.md`,
   "defer : steal closure -> ready-fiber"). The specification's `fiber (reply : Resume) body` form,
   its resume binding, and the single `step` that always takes a value are a drift from that and
   are to be removed. `defer` over a closure gives a dormant `ready-fiber`; `fiber-start` advances
   it with no value; `fiber-resume` advances a `suspended-fiber` with a `Resume`; `yield` evaluates
   to that value, owned. Inputs arrive as captures. Nothing in the body runs at `defer`.
2. **`defer f(args)` evaluates the arguments at the `defer`** and delays only the call (proposed by
   the reviewer, not objected to; confirm).
3. **`spawn` and `defer` return different types**: `task<R,X>` (scheduler advances, `join`,
   `cancel`) and `ready-fiber<Y,Resume,R,X>` (holder advances).
4. **A running task holds a strong reference to its own result object.** When it finishes, its
   stacks are unwound and freed, the result (or failure) is stored in that object, and the task
   releases its reference. If no other reference exists the stored value is dropped then. Dropping
   a task handle therefore does not cancel the task; `cancel` does.
5. **A raw fiber the holder advances has no self-reference.** Dropping it while dormant or
   suspended runs its cleanup at the drop, in the dropping execution's sequence.
6. **A result read by several holders** (the cached-promise case) is a library type over a task
   behind a `Shared` owner: reads borrow, each reader gets a copy of the result or has the stored
   failure raised in its own `try`. A failure is a stored value, never an event that fires with
   nobody listening.
7. **A failure nobody read** is reported when its object is freed: to the global handler if the
   program specified one, otherwise to the host, which shows it. Nothing is raised and the program
   never stops executing because of it.
9. **Work that is waiting on a holder that no longer exists is cancelled.** A fiber suspended at a
   `yield` whose handle (or successor handle) is dropped is cancelled at that drop and its cleanup
   runs there, whether the holder advanced it by hand or a scheduler was feeding its yields to a
   consumer. The self-reference of decision 4 keeps alive only work that can finish on its own. A
   `yield` is legal in an ordinary function body; `defer f(args)` makes that body a fiber.
8. **A local type has no access to the enclosing function's locals**, including from a lambda
   written inside one of its operations. The declaration is a scope barrier for run-time locals;
   outer state comes in through the constructor. (finding 80)

10. **A fiber conforms to `Range`** through the one-element-buffering wrapper the design document
    describes: `empty?` and `front` read the buffer, `pop-front` advances the fiber, and making the
    range primes it. `foreach` over a fiber is the ordinary range lowering. The owner's `done()` is
    `empty?` and `await()` is `front` then `pop-front`; `join()` consumes the handle.
11. **`return v` in a fiber used as a range is its last item** when `v` has the item type; the
    conversion is rejected when the types differ.
12. **Only the fiber itself is a primitive.** Creating a fiber from a callable needs no runtime and
    gives a generator (`fiberCreate(foo)` in the owner's words). `defer` and `spawn` are library
    functions over that primitive and a scheduler, which is created on first use. This renames
    decision 1's construction primitive: what the design document called `defer` is the
    runtime-free creation; `defer` and `spawn` are the scheduled forms. Their exact difference and
    who drives the scheduler are to be confirmed.

13. **Three ways to begin, one contract (agreed).** `start f(args)` runs the body at once in the
    caller up to its first `yield` or `return`; `defer f(args)` runs nothing until first awaited;
    `spawn f(args)` runs it concurrently. All three give a value that satisfies `Range` (single
    pass: `empty?`, `front`, `pop-front`) and `Cancellable`. `start` and `defer` share a concrete
    type and need no scheduler; `spawn` has its own concrete type with a synchronised one-element
    buffer and requires transferable captures and items. A failure before the first `yield` under
    `start` is stored and raised at the first read.
14. **A plain call to a function whose own body contains `yield` returns the lazy range `defer`
    would (agreed).** No function is annotated as asynchronous. The rule is the same inside another
    yielding function: the call makes a range value and forwards nothing, so delegation is written
    out (`foreach (x : inner()) yield x`, or a short form to be named). A function is a generator
    only when `yield` appears in its own body.
15. **Replies go through the same object, as an output side (owner's design).** A fiber whose body
    uses the value of `yield` also accepts `pushReply(v)`; `pop-front` then advances it. The `yield`
    expression has type `Option<Reply>`: `some(v)` when a reply was pushed since the last advance,
    `none` otherwise, so "no reply" and "a reply that is itself empty" stay distinct and the fiber
    must unwrap. Every fiber is therefore a `Range` and `foreach` works on all of them; one that
    takes replies additionally has `reply(v)`, and replying to one that does not is a type error.
    The operation is named `reply`, not `push`, and is specific to fibers (owner). To settle: one
    slot with replacement, or a queue (owner leans to a queue, undecided).
16. **A fiber's reply type is its own parameter list (owner).** `yield` always has type
    `Option<Params>`, where `Params` is the parameter list of the yielding callable: a reply is a
    fresh set of arguments, `none` when the consumer sent nothing. No annotation is added and no
    body scan is needed for the type; the caller reads it from the signature it already has, and
    `reply` takes the same arguments as the call. Every `yield` in one body yields the same item
    type. A generator that wants a reply type other than its arguments is written as a callable
    returned by an outer function, whose own parameter list is then the reply type. Generator-ness
    itself is still decided by `yield` appearing in the body.
17. **`reply` has the generator's own signature (owner).** It takes the generator's parameters and
    returns its item type: `reply(args)` hands the arguments to the waiting `yield` as `some(args)`,
    advances the fiber to its next `yield` or `return`, and returns that value, which is also the
    new `front`. `pop-front` is the same advance with `none`. There is therefore no reply slot and
    no queue. Consequence to confirm: a `reply` inside a `foreach` body advances once, and the
    loop's own `pop-front` advances again. `reply` on a finished generator raises. The unwrap
    spelling for `Option` is still to choose.

18. **`yield` binds like `return`** (`yield a + b` yields the sum). The item type of every `yield`
    and the declared result agree, or the result is `unit` (`F-DIAG-YIELD-TYPE`).
19. **One parameter gives `option<T>`, several an option of their tuple, none an option of the
    empty tuple.**

**Implemented in the reference artifacts** (decisions 4 to 7 and 13's `defer`/`spawn` excepted):
the `fiber (reply : T)` form and `step` are gone from the three grammars, both machines, the IR,
the prelude, and the vectors. In their place: generator functions, `generator-create`, the range
operations `empty?`, `front`, `pop-front`, `reply`, `start`, `yield` evaluating to an option, the
stored failure under `start`, and cancel-with-cleanup when a handle is dropped. Eleven execution
vectors in all three syntaxes, two rejections, and static-pass tests for the type rules.
`SPECIFICATION.md` section 12 states the rules.

Chosen without the owner, to confirm:

- (settled by the owner: reading or advancing a finished generator raises the catchable exception
  `RangeEmpty`, not a trap; two vectors pin it)
- `reply` when the advance ends the generator without a last item raises the same exception, since
  `reply` returns the item type and there is none;
- `some` stops being a reserved word outside type position so it can name the option case;
- range operations are written as calls in the C-like syntax (`front(g)`, `` `empty?`(g) ``) because
  the member-call form `g.front` needs a general rule for calling an operation on a value, which
  the C-like grammar does not have;
- cleanup that suspends while its generator is being dropped is rejected by the reference machines.

Not done: `foreach` in any syntax; the `Range` and `Cancellable` concept conformance and their
prelude signatures; `defer` for a callable that does not yield; `spawn` over a scheduler written in
the language, the task self-reference, and the unread-failure event (decisions 4, 7, 12, 13); a
generator lambda called through a closure value; tuples, so `reply` carries one value; the shared
result type (decision 6).

Why JavaScript's promise problems do not carry over: work exists only behind a handle whose type
carries its exception set; a `defer`red body cannot run or fail before someone advances it; a
task's failure is stored and raised only at a read.

Open: what happens to a task still running when its turn ends (wait, cancel, or carry into the
session); whether local function declarations are capture-free functions or named closures; the
callback contract, which must not let a host run a closure outside any task the program holds.

### 82. Method-call syntax and taking a record apart — DECIDED AND SPECIFIED, NOT YET EXECUTABLE

**Uniform call syntax (owner: wanted, with concepts first).** `value.name(args)` resolves in the
existing member order (field, property, inherent operation, concept operation) and only then as a
free function called with `value` as its first argument. A zero-argument property is read without
parentheses, so a range's `front` and `empty` are properties and `popFront()` and `reply(x)` are
operations. Not implemented: the C-like reader cannot tell a field read from a property read without
types, so this is a resolution rule, not a grammar rule. Until then the range operations are written
as calls.

**Destructuring (owner: no `opDestructure` hook for ordinary records; private fields and
destructors are the problem).** The owner agreed the rule below on 2026-10-03 and it is normative in
`SPECIFICATION.md` section 9 ("Taking a record apart"), together with the two-step drop order and
uniform call syntax as the last step of member resolution. None of it is executable yet: the
reference machines have no visibility, no properties, no drop hooks, and no rest marker in record
patterns, so there are no vectors. The rule:

1. A destructuring pattern may name only what is visible at that point: public fields and public
   `get` properties from outside the type, everything from inside it. A property named in a pattern
   is called, in pattern order.
2. Fields left unnamed must be acknowledged with a rest marker (`...`); they stay in the value.
3. After the named parts are taken, what remains of the value is dropped at once.
4. A type with no destructor gives up its named owned fields by move; the rest are dropped field
   by field as usual.
5. A type with a destructor never has an owned field moved out by a pattern, because its destructor
   would then run over a hole. A pattern on such a type may take copies of `Copy` fields and the
   results of properties, and the value is then dropped whole, destructor included. To hand owned
   parts out, the type's author writes an ordinary consuming operation that returns a plain record,
   and the caller destructures that.

**Failing constructors and drop hooks (owner asked for a concrete rule).** Section 9 now states it.
A value exists once its record literal finishes: a failure before that drops the fields initialized
so far, in reverse, and never runs the drop hook; a failure after it drops a complete value, hook
included. No partially constructed value is observable. A drop hook cannot raise (hooks are
`nothrow`, checked); if one traps, the fields are still dropped and the trap follows the
failing-cleanup rule. The vector `record-construction-failure-drops-initialized-prefix` pins the
field case; constructors and hooks are not executable yet.

A separate, later feature: a refutable, user-defined pattern for values whose shape is known only
at run time (a JSON-like value). That is an extractor that returns an option of the parts, not a
way of consuming a record, and should get its own name and concept.

### 83. Standalone self-contained scripts, decentralized dependencies, and script manifests — DECIDED

**Finding.** Section 4 states that the package graph and root mapping come strictly from a build
manifest/lockfile on disk. This created a contradiction with section 3 and the design history's promise
that Finch scripts are portable, self-contained single-file artifacts: a standalone script importing an
external library could not execute without an enclosing project directory and manifest file.

**Decision (owner, 2026-10-04).**
1. *Direct String/Locator Imports:* For `script` and `submission` roots, `module_path` accepts an
   `escaped_string` in addition to dotted identifiers (for example `(import "github.com/finch-libs/http#v1.2.0" (:as http))`,
   `import: "github.com/finch-libs/http#v1.2.0" import{ as http } ;`, and in C-like syntax
   `import "github.com/finch-libs/http#v1.2.0" as http;`). The alias `:as` / `as` is mandatory for string
   locators because the locator string is not an identifier. The runner verifies and caches immutable
   content hashes in a global user cache without requiring an on-disk project directory.
2. *First-Class `script` Manifest Form:* For scripts requiring granular dependency configuration or
   explicit sandbox capability declarations, a native top-level `script` declaration form is supported
   across all three frontends:
   - CoLisp: `(script (dependencies [http "github.com/...#v1.2.0"] [crypto :source "..." :features ["client"] :flags {:backend "pureFinch"}]) (capabilities (net:out "api.example.com") (fs:read "/tmp/*")))`
   - Co-Forth: `script: dependencies{ http: "github.com/...#v1.2.0" ; crypto: source: "..." features{ "client" } flags{ backend: "pureFinch" } ; } capabilities{ net:out "api.example.com" ; fs:read "/tmp/*" ; } ;`
   - C-like: `script { dependencies { http: "github.com/...#v1.2.0", crypto: { source: "...", features: ["client"], flags: { backend: "pureFinch" } } } capabilities { netOut: "api.example.com", fsRead: "/tmp/*" } }`
3. *Dependency Configuration & Feature Flags:* Dependencies in manifests or `script` blocks may specify
   `features`, `default-features` (or `defaultFeatures`), and compile-time `flags` (e.g. backend selection).
   This allows callers to prune unwanted transitive dependencies and avoid linking against C libraries
   or foreign ABIs (e.g., opting into pure-Finch crypto).
4. *No Comment-Scraping Hacks:* The script manifest is a first-class AST node parsed in the same single
   parse pass as the rest of the source, strictly satisfying the "One Parse Boundary" rule without
   out-of-band TOML/comment scraping.
5. *Conditional Lowering Without `static if`:* Reaffirmed Finding 74: ordinary `if` over compile-time
   constant conditions (including dependency flags) guarantees that only the selected branch is lowered
   to IR. Both branches are type-checked to prevent bitrot, and untaken branches contribute zero linked
   symbols or dead code.

### 84. Diamond dependency resolution and semantic interface compatibility — DECIDED

**Finding.** When dependency A and dependency B both depend on dependency C but specify different
versions, passing a type or concept originating in C between A and B risks compilation failure due to
nominal type identity or concept evidence mismatches. Furthermore, globally additive feature unification
(as in Cargo) can accidentally force unwanted heavy features across unrelated consumers.

**Decision (owner, 2026-10-04).**
1. *Frontend Pre-Compilation Version Agreement:* Before type checking or lowering begins, the frontend
   dependency solver walks the transitive dependency graph and unifies compatible SemVer ranges into a
   single concrete version of C. A and B compile against that single unified instance, preventing
   duplicate identical types in memory.
2. *Isolated Dependency Configurations:* Unlike Cargo's workspace-wide additive feature unification,
   Finch dependency configurations (features and compile-time flags) are isolated per consumer unless
   explicitly unified by the root manifest, preventing an optional feature requested by A from polluting B.
3. *Nominal Type Safety Across Major Versions:* Different major versions of records or variants remain
   distinct nominal types to preserve memory layout and destructor safety. Mismatches produce actionable
   diagnostics naming the package identity, SemVer version, content hash, and origin of each candidate.
4. *Semantic Interface Intersection Verification:* Because Finch module interfaces are sealed, content-hashed
   canonical contracts (Spec §4), the compiler can verify whether an upgraded major version preserves the
   exact subset of types, layouts, and operations consumed by an older caller. When the consumed subset is
   identical, promotion is safe without code changes.
5. *Concept Evidence Bridging:* For types communicating across concepts (interfaces), Finch's decoupled
   explicit evidence passing and `stable-evidence` (#NN operation keys) allow callers to provide bridging
   evidence implementations without modifying either upstream library, avoiding Rust's orphan-rule deadlock.
6. *Public Interface Leak Prevention:* The compiler checks whether a library's public signatures expose
   types from private dependencies, encouraging explicit re-exports or abstract concepts.

### 85. Stackless state machine lowering for fibers, generators, and suspending tasks — DECIDED

**Finding.** Earlier design passages described a resumable execution as allocating a "private operand stack."
If lowered as a stackful fiber (like D's vibe.d or Go goroutines), every fiber, generator, or task allocates
a dedicated stack buffer (typically 4KB–64KB). Under high concurrency (tens of thousands of connections),
this incurs massive memory overhead, cache-line pollution, stack-overflow/growth checks, and context-switching
register save/restore penalties, which often drives runtimes to rely on garbage collection. Conversely, Rust's
stackless futures achieve high density and low latency but impose viral `async`/`await` function coloring.

**Decision (owner, 2026-10-04).**
1. *Stackless State Machine Compilation:* Generators, fibers, and suspending functions compile into compact
   stackless state machine activation records. The compiler statically computes the minimal live variable
   set across all yield/suspension points. The activation record contains only an integer state tag and
   the union of live values across suspension edges.
2. *Zero Dedicated Stack Allocation:* A fiber or suspending function does not allocate a private execution
   stack. When advanced via `front`, `pop-front`, `reply`, or a task scheduler drive step, the execution
   runs directly on the driving thread's existing stack.
3. *Zero Function Coloring in Source Syntax:* Callers do not write `async` or `await` keywords. The compiler
   infers the `suspends` effect from call graphs (Spec §7) and automatically generates the state machine
   transformation. Direct non-suspending calls pay zero continuation or allocation overhead.
4. *Zero Garbage Collection (`@nogc`):* Sizing of activation frames is deterministic at compile time.
   Frames are owned linear values held by the fiber (`fiber<Y,R,X>`) or task (`task<T,X>`) handle and are
   reclaimed deterministically upon completion or cancellation.
5. *Zero Context-Switch Overhead:* Yielding or suspending updates the state tag and returns to the caller
   or scheduler loop (identical to returning a status enum); resuming is a direct function call entering
   a compiler-generated dispatch table. No CPU register-file dumps or stack pointer swaps are performed.
6. *Soundness Without Pinning:* As mandated by Section 8, loans cannot cross suspension points. Variables
   surviving suspension must be owned values (`Copy`, `Unique`, `Shared`, or moved data). This eliminates
   the self-referential pointer problem that forced Rust to introduce `Pin<&mut T>`.

## Cross-cutting conclusions

The architecture is coherent: two readers, one semantic construction boundary, one verifier, and
one execution IR is a strong foundation. The largest risk is not a missing feature; it is letting
the rationale document serve as executable grammar. The second-largest risk is declaring ownership
and effects "typed" without writing the judgments that make them decidable and independently
testable.

The formalization should therefore proceed in this order:

1. lexical and surface grammar;
2. core kinds, types, places, ownership, and effect-row algebra;
3. elaboration and evidence resolution;
4. dynamic semantics and cleanup/control transitions;
5. IR certificate and verifier obligations;
6. paired examples and negative conformance fixtures; and
7. ABI/extensions, each behind a declared profile.

No implementation milestone should treat prose examples as authority once the corresponding
normative rule and fixture exist.
