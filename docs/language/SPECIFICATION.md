# Finch language specification

Version: 0.1-draft  
Status: normative closure draft; implementation and corpus-integration gates are tracked in `IMPLEMENTATION_ROADMAP.md`  
Date: 2026-10-03

## 1. Scope and conformance

Finch is one statically typed language with three source syntaxes: CoLisp, Co-Forth, and a C-like
syntax. The language is defined by what all three construct, not by any one of them: a feature has
a spelling in each syntax or a stated reason for having none, and code written in one is callable
from the others with no adapter because they share one module interface and ABI. The C-like grammar
currently covers the executable core (functions, variants, and expressions); modules, concepts, and
staging forms are specified for CoLisp and Co-Forth only. All
frontends construct the same semantic program, which lowers to the same typed stack IR and is
accepted for execution only after independent verification.

This document is normative. [`FINCH_LANGUAGE_DESIGN.md`](FINCH_LANGUAGE_DESIGN.md) records rationale
and history; [`DESIGN_REVIEW.md`](DESIGN_REVIEW.md) records the closure audit. When an example or
historical passage in either differs from this specification, this specification wins.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, and **MAY** have their usual RFC 2119
meanings.

Stable rule families are assigned by [`spec-rules.json`](spec-rules.json). A fixture cites a
specific `F-<FAMILY>-<NAME>` rule; identifiers are never repurposed after publication.

A conforming implementation declares the profiles it supports:

- **core** — modules, values, functions, ownership, records, variants, concepts, effects,
  exceptions, deterministic cleanup, ranges, and the reference interpreter;
- **staging** — bounded CTFE, reflection, hygienic syntax transformation, generics, parameter packs,
  and value-generic parameters;
- **concurrency** — fibers, tasks, streams, cancellation, checkpointable suspension, and safe
  synchronization;
- **portable-abi** — the versioned host/compiler/runtime ABI and safe foreign adapters;
- **native** — verified IR to native code with interpreter-equivalent behavior.

`accelerator` is reserved for a separately versioned future kernel/tensor extension. It is not a
version 0.1 conformance profile and an implementation MUST NOT claim it as one.

An implementation MUST reject a module requiring an unsupported profile. It MUST NOT silently
reinterpret that module through a legacy evaluator, dynamic fallback, or different source language.

## 2. Compilation and trust boundary

The required phase order is:

```text
source bytes
  -> frontend-private, span-preserving syntax tree
  -> versioned semantic-construction events
  -> declarations and immutable module interfaces
  -> expansion, resolution, typing, ownership/effect checking
  -> parametric HIR where a body is not yet concrete
  -> concrete typed stack IR
  -> independent verification
  -> ModuleVerified
  -> interpreter or verified native lowering
```

Each source byte crosses exactly one reader. Generated code is structured `syntax`; no operation
converts generated text back into source. A frontend cannot mint resolved symbols, evidence,
certificates, or verified modules. Execution consumes `ModuleVerified`, never a frontend tree.

The event schema defines the logical boundary, not a mandatory JSON serialization in the compiler's
hot path. A conforming implementation may stream typed in-memory events directly into construction;
canonical JSON is required only where an artifact or digest is requested. Likewise, execution
transitions are semantic states, not a requirement to allocate, stringify, or log one object per
`Continue`. Verified pure code carries no runtime effect-row dictionary, evidence selected statically
may be specialized away, and checks whose predicates are proven by verified IR may be eliminated.
Portable-ABI encoding, checkpoint encoding, grapheme segmentation, atomic reference counting, and
effect journaling occur only when the corresponding explicit feature is used.

The normative performance acceptance targets are machine-readable in
[`performance-budgets.json`](semantics/performance-budgets.json). They are release gates, not claims
that the current implementation meets them. A result without the required machine/toolchain/corpus
manifest is not evidence. Exact zero-tax counters are semantic requirements; elapsed/RSS thresholds
must be measured with the pinned manifest and paired correctness assertions.

The canonical frontend boundary is [`semantic-events.schema.json`](schemas/semantic-events.schema.json).
Event streams and interfaces use the domain-separated canonicalization and digest rules in
[`canonical-digests.json`](semantics/canonical-digests.json). A frontend-specific tree or source
location never participates in the paired-syntax semantic digest.
[`semantic-event-kinds.json`](schemas/semantic-event-kinds.json) closes the required and optional
attributes and their JSON types for each kind. Events are preorder nodes; `parent_id`, `role`, and
`index` identify the exact normalized-AST edge. Floating literal attributes use an object containing
their declared width and lowercase IEEE bit string, never a JSON floating number; other literals use
the schema's integer, Boolean, null, string, or structured canonical representation.
Before an interface digest is computed, exports are sorted by `(namespace, identity, kind)`,
dependencies by `(module, phase, interface-digest)`, and unsafe-summary strings by Unicode scalar
order. Duplicate export or dependency sort keys are invalid, so source declaration/import order
cannot perturb interface identity.

Incremental dependency fingerprints name only exported contracts actually resolved by the client.
An exported body has separate semantic, inline, and CTFE-body identities; changing an unused export
or a non-inlined body MUST NOT invalidate an otherwise reusable client type-check result. Whole-
interface digests remain publication identities, not mandatory coarse rebuild keys.

The semantic scheduler uses monotonic symbol states:

```text
Declared < SignatureReady < BodyTyped < Lowered < FunctionCertified
ModuleParsed < ModuleElaborated < ModuleSealed < ModuleVerified
```

For a function `SignatureReady` is its sealed signature, `BodyTyped` its typed body, `Lowered` its
IR, and `FunctionCertified` that IR verified. For a type `SignatureReady` is its sealed generic
header, `BodyTyped` its typed member list, and `Lowered` its layout, with which it is also
certified. A module is `ModuleParsed` when its source is read, `ModuleElaborated` when its
declarations are entered as `Declared`, `ModuleSealed` when every symbol is `SignatureReady`, and
`ModuleVerified` when every symbol is `FunctionCertified`. A compiler job is the work of bringing
one symbol to one state; it is bounded because it is charged against the compile-time budget of
section 11.

`require(identity, state)` either returns the immutable result, suspends the current compiler
job on that dependency, or reports a deterministic cycle/failure. Scheduling order MUST NOT change
the sealed interface, verified IR, or primary diagnostic.

[`compile-scheduler.json`](semantics/compile-scheduler.json) fixes what each operation requires and
what it records:

| A declaration that... | requires of the other symbol | and depends on its |
|---|---|---|
| names it | `SignatureReady` | contract |
| constructs it, reads its fields, or matches its cases | `BodyTyped` | members |
| contains its value inline | `Lowered` | members |
| calls it at run time | `SignatureReady` | contract |
| computes a signature constant by calling it | `FunctionCertified` | body |
| computes a constant in its body, or expands a `mixin`, by calling it | `FunctionCertified` | body |
| reads its signature or generic header by reflection | `SignatureReady` | contract |
| reads its fields or cases by reflection | `BodyTyped` | members |
| reads its body as syntax | `Declared` | body |
| inlines it when lowering | `BodyTyped` | inline body |

A mention needs the named symbol's header, not merely its existence: whether the `x` in `Pair<x>`
is a type or a constant is decided by the kind of `Pair`'s parameter. A mention of the symbol being
declared is met by its declaration. A mention in a member list needs only the other header, so
records whose fields name each other are not a cycle, while a type that contains itself inline is
one, reported where its layout is computed. A requirement written in a signature is issued while
that signature is being sealed and one written in a body while the body is typed, whichever kind it
is. A run-time call needs only the callee's contract, so mutual recursion is not a cycle and editing a
body does not recheck its callers. A compile-time call needs the callee certified, and then each
callee the evaluation actually executes, on demand; the caller then depends on those bodies. An
inferred signature is sealed by typing its body, so it issues every requirement of that body while
it is being inferred, which is why a definition that takes part in a cycle needs an explicit one.

The outcome is defined as a fixed point rather than by any order of work. A symbol whose own work
fails keeps the last state it reached and reports its own diagnostic. When no job can proceed,
each unmet goal waits on exactly one other; every cycle in that relation is one
`F-DIAG-COMPILE-CYCLE` listing its goals from the smallest in (symbol, state) order, and a goal that
waits on a failed or cyclic goal reports `F-DIAG-DEPENDENCY-FAILED`, which is never the primary
diagnostic. [`compile_scheduler.py`](../../scripts/language/compile_scheduler.py) is the reference
model, and [`compile-scheduler.json`](fixtures/compile-scheduler.json) holds its cases; the
conformance check runs every case under many scheduling orders and requires one outcome. The
recorded dependencies are what an incremental build replays: a change to a body reaches only the
symbols that recorded a `body` dependency on it.

## 3. Source identity and lexical rules

Source text is UTF-8. Spans use byte offsets at validated token boundaries and retain source identity,
line/column projections, and expansion ancestry.

The complete lexer/parser artifacts are [`common.json`](grammar/common.json),
[`colisp.json`](grammar/colisp.json), [`coforth.json`](grammar/coforth.json), and
[`clike.json`](grammar/clike.json), validated against
[`grammar.schema.json`](grammar/grammar.schema.json). Their exact EBNF/scanner metalanguage is
[`notation.json`](grammar/notation.json). They are normative where the prose below uses
abbreviated schemas; a mismatch is a specification defect, not implementation discretion.

The notation is `Finch-PEG-1`, a parsing expression grammar read parser-directed. Alternatives are
ordered and the first that succeeds is the result; repetition is greedy; a terminal is matched only
where a production asks for it, so token definitions never compete globally and carry no
precedence. Skip tokens are consumed before each terminal. A terminal that ends in an identifier
character must be followed by end of input, whitespace, a delimiter, or a line comment. `identifier`
never matches a reserved word of the frontend or of the shared grammar, which is what prevents a
malformed reserved form from being read as a call or word. Left recursion is a grammar defect that
is rejected when the grammar is loaded. An implementation may use any parsing technique that
accepts exactly the same sources with the same tree and spans.

The grammars are executable. [`grammar_engine.py`](../../scripts/language/grammar_engine.py)
interprets the three artifacts directly, and [`grammar-corpus.json`](fixtures/grammar-corpus.json)
is the accept/reject corpus: every production of both frontends is exercised by at least one
accepted source, and every rejected source names its stable reader code and byte offset. The reader
codes are `F-LEX-INVALID-UTF8`, `F-LEX-IDENTIFIER-NOT-NFC`, `F-LEX-UNTERMINATED-STRING`,
`F-LEX-INVALID-ESCAPE`, `F-LEX-UNBALANCED-COMMENT`, and `F-LEX-SYNTAX`; the offset of
`F-LEX-SYNTAX` is the farthest byte at which a terminal was attempted and failed.

### 3.1 Common identifiers and type expressions

An alphanumeric identifier follows Unicode UAX #31 `XID_Start XID_Continue*`, with `-`, `?`, and
`!` additionally allowed after the first character. Every identifier and logical module-path
component MUST already be Unicode NFC; readers reject rather than silently normalize another
spelling. Identity comparison is case-sensitive scalar comparison. The Unicode data version is part
of the language manifest and cache key. The standalone operator identifiers are `+`, `-`, `*`, `/`,
`==`, `!=`, `<`, `<=`, `>`, and `>=`. `.` and `::` are separators, not identifier characters.
Reserved delimiters and keywords are not identifiers. Module paths are dot-separated alphanumeric
identifiers. The language imposes no case-style convention: case is identity, while linters may
recommend a project convention without changing acceptance or linkage.

CoLisp word-like tokens (identifiers, keywords, numerics, Boolean/character atoms, and raw-string
prefixes) must end at whitespace, a declared delimiter, or end of input; the reader never splits an
otherwise contiguous invalid word into several valid tokens. Co-Forth applies the stronger
whitespace-token rule in section 3.5. Quoted strings and punctuation delimit themselves as stated by
their productions.

Integer tokens use decimal digits or the `0b`, `0o`, and `0x` radix prefixes. `_` may separate
digits but may not lead, trail, double, or touch a radix prefix or suffix. The optional suffix is one
of `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, or `u64`. An unsuffixed integer is represented
without truncation during reading, elaborates to an expected integer type when one exists, and
otherwise defaults to `int`; failure to fit is a type diagnostic. For boundary-safe lexing the reader
recognizes adjacent `-` and digits as the `negative_numeric` composite fixed by the grammar, but
it MUST normalize that composite to unary negation rather than create a signed-literal semantic
node. The folded minimum signed value is checked against the target type before positive-literal
overflow is diagnosed.

A floating token is decimal, contains a decimal point or `e`/`E` exponent, and may end in `f32` or
`f64`; unsuffixed values default to `float`. The significand and exponent digits use the same
underscore rule. Hexadecimal floats and source literals for infinity or NaN are absent in 0.1;
explicit library constructors or representation operations create them. The reader converts a
literal directly to its target width using nearest/ties-even and MUST NOT first round through `f64`.

A CoLisp character literal is `#\x`, `#\space`, `#\newline`, `#\tab`, or `#\u{H...}`. A Co-Forth
character literal is `c"x"` or `c"\u{H...}"`. After escape processing it MUST contain exactly one
Unicode scalar. Character literals are distinct from one-scalar strings.

Both frontends use the same dedicated type-expression grammar:

```ebnf
type          = primary-type, { generic-args | projection } ;
primary-type  = callable-type | qualified-name | "&", type | "&mut", type | "dyn", type | "some", type ;
callable-type = "callable", "<", "(", [ callable-param, { ",", callable-param } ], ")", "->", type,
                [ contract ], ">" ;
callable-param = [ "borrow-mut" | "steal" ], type ;
generic-args  = "<", type-arg, { ",", type-arg }, ">" ;
type-arg      = type | const-expression | associated-binding | refined-path-argument ;
projection    = "::", identifier ;
associated-binding = identifier, "=", type-arg ;
refined-path-argument = qualified-name, ":", string ;
const-expression = const-sum ;
const-sum      = const-product, { ("+" | "-"), const-product } ;
const-product  = const-primary, { ("*" | "/"), const-primary } ;
const-primary  = integer | negative-numeric | identifier | "(", const-expression, ")" ;
```

A callable type is written only in this arrow form: ordered parameter types with their ownership
modes (unmarked is a readonly borrow), the result type, and a contract in the same syntax and with
the same axes as a declaration's contract, for example
`callable<(int, steal Token) -> int ! effects<e> | throws<X> | suspends>`. An omitted contract is
inferred and is permitted only where a declaration's contract may be omitted; a published signature
states all three axes. `plain` and `inferred` normalize away exactly as they do on declarations.
`callable` is reserved and is not an ordinary generic constructor in source. The prelude's kind-level
constructor `callable<P, R, e, X, s>` is the same type seen as five components: the parameter list,
the result, the effect row, the exception set, and the suspension constant; in prelude signature
notation `unit` in the first position means no parameters and a single type means one borrowed
parameter.

Whitespace is allowed around `<`, `>`, `,`, `=`, and `::`. `>>` in a nested generic expression is
two close tokens, never a shift token. `T::Item` is the only associated projection spelling. A bare
name in a generic argument is one unresolved name node; the referenced constructor's parameter kind
deterministically resolves it as a type or constant, so parser alternative order is unobservable.

### 3.2 Strings

Escaped strings support `\\`, `\"`, `\n`, `\r`, `\t`, `\0`, `\xNN`, and `\u{H...}`. `\xNN`
denotes the scalar U+00NN, encoded as UTF-8 in the resulting value. A Unicode
escape MUST denote a scalar value and MUST NOT denote a surrogate or a value above `0x10ffff`.
Invalid UTF-8, escape digits, or delimiters are reader errors.

Raw strings use `r"..."`, `r#"..."#`, or additional matching hashes. Their content is literal UTF-8
and ends only at a quote followed by the opening hash count. Co-Forth additionally accepts `s"..."`
as an escaped-string compatibility spelling. If an ASCII space immediately follows the opening
`s"`, exactly that one space is a delimiter and is not content; otherwise content begins
immediately. Nothing is consumed after the closing quote.

`"""..."""` (and Co-Forth `s"""..."""`) remove one initial CRLF or LF when present. Indentation is
the count of leading ASCII spaces on each nonblank remaining line; tabs and other whitespace are
content and therefore make that line's indentation zero. The reader removes the minimum such count
from every line, including blank lines that contain at least that many spaces, and otherwise
preserves content and line endings. They produce ordinary `string` values. A triple-quoted string
processes the same escapes as an escaped string before indentation is removed, and it ends at the
first unescaped `"""`.

`true`, `false`, and `null` are reserved literal spellings in both frontends and cannot be bound.

### 3.3 Canonical declaration forms

The following are syntactic schemas. `name*` means zero or more repetitions in source order; square
brackets in this table denote optional grammar and are not literal tokens.

| Semantic node | CoLisp | Co-Forth |
|---|---|---|
| function | `(define (name [<generic*>] param*) : result [contract] body+)` | `: name [< generic* >] ( stack-in -- stack-out [contract] ) body* ;` |
| lambda/quotation | `(lambda [capture-spec] (param*) [contract] body+)` | `[ [capture-spec] ( stack-in -- stack-out [contract] ) \| body* ]` |
| record | `(record Name [<generic*>] [(repr mode)] field*)` | `record: Name [< generic* >] repr(mode) fields{ field* } ;` |
| field | `(field [pub\|pkg] name : type)` | `[pub\|pkg] name: type` |
| variant | `(variant Name [<generic*>] case*)` | `variant: Name [< generic* >] cases{ case* } ;` |
| concept | `(concept Name [modifier*] [<generic*>] requirement*)` | `concept: Name [modifier*] [< generic* >] requirement* ;` |
| implementation | `(implementation [<generic*>] Type [: Concept] member*)` | `implementation: [< generic* >] Type [: Concept] member* ;` |
| lexical import | `(import module-ref binding*)` / `(from module-ref :import binding*)` | `import: module-ref import{ binding* } ;` / `from: module-ref import{ binding* } ;` |
| export | `(export name*)` | `export: name* ;` |
| test suite | `(test-suite "name" test*)` | `test-suite: "name" { test* }` |
| test | `(test "name" (ctx) body+)` | `test: "name" ( S borrow-mut TestContext -- S ) { body* }` |

A CoLisp `param` is `(name : type)`, `(borrow-mut name : type)`, or `(steal name : type)`. A generic
entry is `name [: bound [+ bound]*] [where expression]`, `value name : type [where expression]`,
`effect name`, `exceptions name`, `region name`, `stack name`, `infer name : kind`, `(types names...)`,
`(values names : types...)`, or `(params names...)`. An unadorned name has kind `Type`; `infer`
declares an output solved by argument/evidence matching rather than supplied by the caller. The four
named entries bind `EffectRow`, `ExceptionSet`, `Region`, and `StackRow` respectively. An operation
name is an identifier or one of the operator identifiers. A CoLisp `let` binding is
`[mut] name [: type] initializer`; the optional type is the counterpart of a Co-Forth local's type
and participates in the semantic digest. A concept modifier is `symmetric`,
`commutative`, or `stable-evidence`; the last requires `(evidence-version integer)` and author-keyed
operations `(operation #integer signature)`.

A CoLisp `capture-spec` is `:move` or `(:captures [:move] capture-entry*)`. A capture entry is
`(borrow name)`, `(borrow-mut name)`, `(steal name)`, `(retain name)`, `(weaken name)`, or
`(copy name)`. Without a capture spec, only inferred scoped loans in a proven nonescaping,
nonsuspending closure are permitted. `:move` supplies the owning default for every used free binding;
entries in `(:captures :move ...)` override that default, while `(:captures ...)` is exact and rejects
an unlisted free binding.

The Co-Forth equivalents are `captures: move`, `captures: { mode name ... }`, and
`captures: move { mode name ... }` before the stack signature. They normalize to the same ordered
capture entries. Capture order in IR is deterministic by resolved binding identity, not source hash
or map iteration order.

The canonical contract syntax is:

```ebnf
contract      = "!", contract-item, { "|", contract-item } ;
row-variable  = identifier ;
state-target  = identifier | "arg(", identifier, ")" | "result" ;
contract-item = capability-request
              | "plain" | "inferred"
              | "pure" | "effects<", [ row-variable ], ">" | "effects-infer" | "nothrow"
              | "state<", state-target, ">" | "state-read<", state-target, ">"
              | "returns-loan<arg(", identifier, ")>"
              | "throws<", type, { ",", type }, ">" | "throws-infer"
              | "non-suspending" | "suspends" | "suspends-infer"
              | "comptime" ;
```

Inside `throws<...>`, comma-separated type names form a concrete exception antichain. A single bare
name bound by `exceptions X` instead denotes that `ExceptionSet` parameter; the parser emits an
unresolved exception-summary name and kind resolution makes the choice. `throws<>` is the empty set.

`plain` is reader sugar for `effects<> | nothrow | non-suspending`. It describes ordinary bounded
local work but does not assert referential transparency. `inferred` is publication sugar for
`effects-infer | throws-infer | suspends-infer`. Both normalize away before semantic construction,
so interfaces, overloads, and digests contain only the three explicit axes. They cannot be combined
with another item from an axis they supply.

If any predicate/exception/suspension item is written, both exception and suspension axes MUST be
present; capability-only private declarations may omit those axes and infer them. Published
declarations may use the three `*-infer` forms to freeze inferred rows and summaries. Capability
requests use `namespace.operation{name=selector,...}` with the selector grammar in section 14.

`effects<>` declares an empty residual effect row without asserting the stronger referential-
transparency guarantee of `pure`. `effects<e>` includes the named `EffectRow` parameter as an open
row tail. `effects-infer` freezes
the exact inferred, possibly parametric effect-row expression into a published interface.
`state<r>` and `state-read<r>` name writes and reads of a lexical region bound by `region r` or
introduced freshly by the checker. `arg(name)` ties a region to mutable state reachable from that
parameter; sealed interfaces replace the source name with its parameter ordinal. `result` names
fresh state escaping in the result. Substitution alpha-renames fresh regions, unions aliases that
the call binds to the same actual place, and canonicalizes region rows by ordinal before hashing.
These labels carry no host authority and are never capability requests.

`returns-loan<arg(name)>` is required when the result contains a loan. It records the single input
owner to which every returned loan traces, uses the parameter's resolved ordinal (not its spelling)
in sealed interfaces and hashes, and is rejected when the result has no loan or when any result loan
could trace to another origin. It is ownership metadata, not an effect-axis alternative.

The alternatives on each closed axis are mutually exclusive. A contract cannot contain both
`nothrow` and a `throws` item, both `non-suspending` and a `suspends` item, duplicate closed items, or
`pure` together with a nonempty residual effect request. `effects-infer`, `throws-infer`, and
`suspends-infer` mean that the exact inferred row or summary is written into the sealed interface;
changing one is then an interface change even though the source retained the `*-infer` spelling.

Member schemas are `(constructor (name param*) : result [contract] body+)`,
`(get (name (self)) : result [contract] body+)`, `(set (name (borrow-mut self) param) : unit
[contract] body+)`, `(operation [#key] signature [body*|target])`, and
`(associated name [value])`; Co-Forth uses the corresponding `constructor:`, `get:`, `set:`,
`operation:`, and `associated:` entries terminated by `;`.

Convenience syntax is conforming only when reader normalization produces exactly one of these nodes
before expansion. A parser MUST reject a reserved form with the wrong schema rather than treating it
as an ordinary call/word.

### 3.4 CoLisp reader

CoLisp uses:

- `(...)` for calls and core forms;
- `[...]` for sequence syntax selected by the enclosing production, including flat binding lists and
  tuple values;
- `'form` for context-free datum;
- `` `form `` for scope-aware syntax quotation;
- `,form` and `,@form` only inside syntax quotation;
- `@Name form` as reader sugar for `(mixin (Name form))`; and
- `;` to end-of-line for comments.

Record construction is `(Type :field value ...)`; field labels are unique and are evaluated in
source order. `(yield value)` is legal in the body of a function or lambda and makes that callable a generator
(section 12); there is no separate fiber form.

`[...]` is never classified by speculative parse success. Embedded JSON MUST be explicitly tagged:
`json[...]` for an array or `json{...}` for an object. The balanced contents are parsed once by the
JSON grammar. Invalid JSON cannot fall back to Finch syntax.

### 3.5 Co-Forth reader

Co-Forth is whitespace-tokenized except inside strings and balanced structured forms. `\\` starts a
line comment. Parenthesized comments are accepted only in explicitly tagged Co-Forth source; in the
compact untagged provider transport, a leading `(` selects CoLisp.

In explicitly tagged Co-Forth, a balanced parenthesized region is a comment everywhere except where
a production asks for the `(` terminal itself: a stack signature, a constructor pattern's arguments,
a tuple variant's payload, a selector, or a parenthesized constant expression. There `(`
deterministically opens that construct. Comments nest, skip quoted strings, retain no semantic node,
and an unmatched opening delimiter is a reader error at that delimiter.

The following balanced forms are reader-owned and retain nested spans: signatures `( ... -- ... )`,
locals `{ ... -- ... }`, quotations `[ signature | body ]`, `syntax[ ... ]`, `args{ ... }`,
`rest{ ... }`, record/variant constructors `Name{ ... }`,
test bodies, and `unsafe[ ... ]`. In a word or quotation body, `value yield` consumes the yielded
value and, when execution continues, leaves the option of a reply on the stack; a word whose own
body yields makes a generator when called, so it leaves one value whatever result it declares.
They are not ordinary words that search backward through an unbounded stack.

Co-Forth semantic construction rebuilds the same expression tree CoLisp writes directly. The
following rules are normative; [`elaborate.py`](../../scripts/language/elaborate.py) is their
reference and every paired execution vector checks them.

- A word with stack effect `( a1 ... an -- r )` consumes the `n` most recent pending values as its
  arguments in push order and yields one value. `unit` occupies no stack cell: a word or body whose
  result is `unit` yields nothing, and a signature with no output has result `unit`.
- A word that yields nothing is a statement. A body is the sequence of its statements followed by at
  most one final value; a body that leaves more than one value, or leaves a statement after a value
  that is still pending, is rejected. A statement issued while an older value is pending is
  evaluated immediately before the next value that follows it, which preserves source order.
- `drop` applied to a pending value makes that value a discarded sequence item, the same node as a
  non-final CoLisp `begin` item.
- A locals block `{ a b -- c }` binds each name before `--` to a pending value, rightmost name to the
  most recent value, and scopes them over the rest of the enclosing body, equivalent to nested `let`
  in name order. A name after `--` is declared uninitialized: it MUST be `mut`, its first use MUST be
  a `to name` in the same body, and that assignment is the binding's initializer. A local's type is
  optional and corresponds to the optional type of a CoLisp binding.
- `if` without `else` has the unit literal as its else arm. Both arms of `if`, all arms of `match`,
  and a `try` body with its catch arms must leave the same number of values unless an arm diverges.
- An unmarked stack-signature entry is a readonly borrow, exactly like an unmarked CoLisp parameter.
  `return` consumes the callable's single result when it has one.
- The range words on a generator are `empty?`, `front`, `pop-front`, `start`, and `reply`. In 0.1
  `reply` takes the generator and one value; a generator with several parameters needs a tuple,
  which the executable core does not have.
- A word that is not a local, a visible declaration, a reader form, or a host or prelude operation
  has no known stack effect, and construction rejects it with `F-DIAG-UNBOUND-NAME`.
- Functions and values share the value namespace, and a name behaves the same wherever its
  callable came from. A name bound to a callable, whether by `:` or as a local or parameter of
  declared callable type, is applied when written: it consumes its arguments
  and yields its result. `' name` pushes the callable itself without applying it, which is the node
  CoLisp writes as the bare name. `call` applies the callable on top of the stack to the arguments
  beneath it, which is the node CoLisp writes as `(callee argument...)` with a non-name callee. The
  callable given to `call` must have a statically known signature: a quotation, a ticked name, or a
  value whose declared type is a callable type.
- Building the tree requires each applied word's stack signature, so Co-Forth semantic construction
  runs with the signatures of the names in scope already resolved. It needs no types beyond those
  signatures.

### 3.6 C-like reader

The C-like syntax spells the same programs with infix operators, braces, and declarations in the
manner of C and D. [`clike.json`](grammar/clike.json) is its grammar; these rules state how it maps
to the shared program.

- **Names.** An identifier is `XID_Start XID_Continue*` with no `-`, `?`, or `!`, so `a-b` is a
  subtraction. An identifier whose first character is not one of `A`-`Z` is camelCase for the
  canonical name: each of `A`-`Z` that directly follows one of `a`-`z` or `0`-`9` becomes `-` and
  its lowercase form, so `readFile` is `read-file` and `readHTTPFile` is `read-hTTPFile`. The rule
  is defined on those ASCII ranges only; every other character, cased or not, is kept as written.
  An identifier that begins with `A`-`Z` is canonical as written. A name camelCase cannot
  spell is written verbatim between backticks, as in `` `even?` ``.
- **Items and blocks.** A block or submission is a list of items separated by `;`. Its value is the
  value of its last item, and a `;` after the last item changes nothing. A block with one item is
  that item; an empty block is the empty sequence. A local declaration `[mut] Type name = value;` or
  `[mut] auto name = value;` scopes every item after it in the block, exactly as a `let` whose body
  is the rest of the block. A function or variant declaration is an item of the submission
  wherever it is written among the top-level items; one written after a local declaration is not
  inside that local's scope and cannot name it.
- **Expressions.** `+ - * /` and the six comparisons are calls to the operator of the same name.
  `* /` bind tighter than `+ -`, which bind tighter than the comparisons; `+ - * /` associate to
  the left, and a comparison does not associate, so `a < b < c` is a syntax error. Prefix `-` is
  unary negation. `f(a, b)` is a
  call, `p.x` a member read, `x = v` an assignment. `if (c) a else b`, `while (c) body`,
  `match [move] (v) { pattern => value, ... }`, `try value catch (pattern) value`, and
  `scope { onExit g; onSuccess g; onFailure g; onCancel g; items }` are the shared forms; `if`
  without `else` has the unit literal as its else arm, and `()` is the unit literal. `return`,
  `throw`, and `yield` take one expression. `Name { field: value, ... }` constructs a record.
- **Declarations.** `Result name(parameters) attributes { items }` declares a function, and
  `variant Name { Case, Case(Type) }` a variant. A parameter is `Type name`, `mut Type name` for an
  exclusive borrow, or `move Type name` for ownership transfer.
- **Contracts.** A contract is a list of trailing attributes rather than a `!` clause: `plain`,
  `inferred`, `pure`, `nothrow`, `suspends`, `nonSuspending`, `effectsInfer`, `throwsInfer`,
  `suspendsInfer`. No attributes means the contract is inferred.
- **Types.** A generic application is `name!(arguments)`. A callable type is
  `Result function(parameter types) attributes`, the same type as
  `callable<(parameters) -> Result ! contract>`. A bracket suffix builds an array type from the type
  to its left, and one bracket is one array: `T[N]` is `array<T,N>`, `T[]` is `vector<T>`, and a
  dimension list `T[a, b, c]` is the shaped `static-array<T,a,b,c>`. Suffixes compose, so
  `int[3][4]` is `array<array<int,3>,4>`, an array of four arrays of three. Only the dimension list
  denotes a single contiguous block across dimensions.
- **Closures and fibers.** `[captures](parameters) attributes => value` is a lambda. The capture list
  is omitted for inferred scoped borrows, `[move]` for the owning default, and otherwise lists
  `mode name` entries; a list without `move` is exact. A function or lambda whose own body contains
  `yield` is a generator; `yield` binds like `return`, so `yield a + b` yields the sum and
  `(yield a) + b` needs its parentheses. The range operations are written as calls: `` `empty?`(g) ``,
  `front(g)`, `popFront(g)`, `start(g)`, `reply(g, args)`.

### 3.7 Submission identity

The canonical envelope has `language = "lisp" | "forth" | "c"`. The C-like syntax has no compact
shorthand and is always tagged explicitly. The compact provider transport MAY use
first-non-whitespace `(` for CoLisp and any other byte for Co-Forth. Files, scripts, tools, storage,
and IPC MUST carry an explicit language and MUST NOT use that shorthand.

## 4. Modules and packages

A module identity is its normalized path relative to its owning package root. The package graph and
root mapping come from the build manifest/lockfile; source files contain no restatable module-name
declaration. `.colisp` and `.coforth` are equivalent module source extensions. Two files resolving to
the same module identity are an ambiguity error.

The loader resolves the owning package root to one physical directory, rejects a source path whose
resolved target escapes that root, and derives logical identity from the NFC, case-sensitive
manifest-relative path rather than host filesystem comparison. A package is rejected if two source
paths collide after NFC, Unicode default case folding, or physical-file identity. This makes the
same package graph either valid with one identity on every supported host or invalid everywhere;
case-insensitive filesystems do not silently select a winner.

A directory is a package namespace. `package.colisp` or `package.coforth` is its optional package
facade. Visibility is private to the declaring module by default, `pkg` to the declaring package
subtree, and `pub` to clients. `pkg.foo` names a sibling/descendant relative to the current package;
`pkg.super.foo` moves outward one package per `super` without escaping the owning package root.

Imports are immutable lexical declarations. Whole-module, alias, selective, and renamed imports are
permitted. A local import becomes visible after its declaration, only in that lexical scope, and
cannot be re-exported. Importing never initializes state or grants authority. Repeated imports intern
one immutable module identity/job and create additional binding views.

Name resolution searches direct lexical declarations before imported candidates and searches
imports from the innermost lexical scope outward. Two imported candidates in the same scope are
ambiguous regardless of import order. Qualification, selection, or renaming resolves the ambiguity.
An inner import may hide an outer imported candidate but never a direct binding in its own scope.
Local imports cannot be re-exported. A sealed interface records the exact identity and phase of every
resolved imported binding and concept implementation.

The source envelope selects a root kind before parsing. A `library` root accepts declarations only;
loading or importing it executes no runtime expression. An `executable` root is also declaration-
only and the package manifest must name exactly one exported `main` callable; its complete signature,
effect grants, result-to-exit mapping, and output sinks are manifest-checked. A `submission` or
explicit `script` root may contain top-level expressions: they lower in source order into one
implicit private entry callable whose contract is inferred and checked against the submission
policy. Root kind is part of source identity and cannot be guessed from parse success. Tests remain
declarations and never module initialization.

In a `script` or `submission` root, dependencies MAY be declared directly without an external manifest:
- A `module_path` in an import form accepts an `escaped_string` locator (e.g. `"github.com/org/repo#hash"`).
  An explicit alias (`(:as name)` in CoLisp, `as name` in Co-Forth and C-like) is REQUIRED for string locators.
- Alternatively, a top-level `script` declaration form MAY declare a `dependencies` block and an optional
  `capabilities` block. Dependencies declared in a manifest or `script` form MAY configure `features`,
  `default-features`, and compile-time `flags`.
- The runner resolves, content-verifies, and caches remote locators in a content-addressed global cache.

Before compilation or lowering, the dependency resolver unifies compatible SemVer ranges across the
transitive graph into a single compiled module instance. Dependency configurations (`features`, `flags`)
are isolated per consumer unless unified by the root manifest. When different major versions are required,
records and variants remain distinct nominal types. If an upgraded major version preserves the exact
layout, signature, and identity of the subset of declarations consumed by an importer (verified against its
sealed canonical interface), the compiler MAY unify the dependency. Callers MAY supply explicit concept
evidence bridging types across incompatible package versions without modifying upstream packages.

There are three lexical namespaces: **module**, **type**, and **value**. Record and variant types,
type aliases, and concepts occupy the type namespace. Runtime values, functions, constructors, and
compile-time transformer bindings occupy the value namespace; phases do not create shadow bindings
with the same value name. A module alias occupies the module namespace. Importing a declaration
binds it in its declaration namespace, so importing a type from a module does not create a value of
that name unless a separately exported constructor or function is also imported. Fields, properties,
and operations live in the owning type's member namespace and do not become lexical bindings.
Duplicate direct declarations in the same scope and namespace are errors; the same spelling MAY
exist once in different namespaces because its syntactic position selects the namespace before
lookup.

Modules contain immutable declarations. Mutable runtime state is an explicitly constructed owned
value or service instance. There is no ambient module-unload phase.

Interfaces contain exported signatures, nominal identities, evidence identities, layout contracts,
effect/exception/suspension contracts, dependency identities, and compatibility hashes. The same
source/dependency inputs MUST produce byte-equivalent canonical interfaces.

## 5. Kinds, types, and values

The required kinds are `Type`, `Const<T>`, `EffectRow`, `ExceptionSet`, `StackRow`, `TypePack`,
`ValuePack<T...>`, `ParamPack`, and `Syntax`. A generic parameter declares its kind. There is no
implicit conversion between kinds.
`Syntax` is the kind of compile-time syntax parameters and retained form packs; the ordinary
compile-time value type spelled `syntax` still has kind `Type`. Runtime code cannot construct or
inspect a `syntax` value, and a callable mentioning it must satisfy `comptime`.
The intrinsic compile-time domain types `suspension`, `root-kind`, `selector-pattern`,
`path-selector`, `resource-kind`, and `capability-kind` exist only to kind-check `Const<...>`
parameters. A `path-selector` is the single normalized root/pattern pair spelled
`ROOT:"pattern"`; possessing a constant in any of these domains does not itself grant authority or
create a runtime resource.

Core types include `unit`, `bool`, sized signed/unsigned integers, `f32`, `f64`, `char`, `string`,
`bytes`, `array<T,N>`, `slice<T>`, `slice-mut<T>`, `vector<T>`, `list<T>`, `map<K,V>`, `tuple<T...>`,
nominal records and variants, callables, owners, tasks/fibers/streams, resources/capabilities, and
explicit `dynamic`. `int`, `uint`, and `float` alias `i64`, `u64`, and `f64`.

`throws<E...>` is a canonical closed exception-set type argument, normalized to a subtype antichain;
`throws<>` is empty. The resumable handle types are `task<T, X>`, `stream<T, X>`, and
`fiber<Y, Resume, R, X>`, where `X` has kind `ExceptionSet`. `stream-step<T,X>` is the closed
`item(T, stream<T,X>) | done` result of advancing a stream. A fiber is advanced in place through the
range operations of section 12 and has no step-result type.
`exception-value<X>` reifies one value whose dynamic exception type is a member of exception set
`X`; `task-outcome<T,X>` is `returned(T) | raised(exception-value<X>)`.
`remaining-tasks<T,X>` is the sole affine owner of a selected operation's unconsumed tasks, and
`task-selection<T,X>` contains an input index, `task-outcome<T,X>`, and that owner. There are no exception-erasing one-
argument aliases: a producer's typed failure contract remains visible wherever its handle travels.

`never` is the uninhabited result type of expressions that do not continue, including `throw`,
`rethrow`, and a proven non-returning loop. It has no values or storage representation and coerces to
the expected result type at a control-flow join.

Named records and variants are nominal. Tuples are structural. Matching fields do not implicitly
convert records. A readonly structural view requires explicit evidence and is a borrow, not a layout
reinterpretation.

Version 0.1 has no class or implicit record-width subtyping. Representation-preserving subtyping
contains only:

- reflexivity and transitivity;
- covariance of readonly borrows and readonly owner views, but only when the referent relation is
  itself representation-preserving;
- invariance of mutable borrows and mutable ownership containers;
- contravariant callable inputs and covariant results;
- callable contract narrowing: fewer effects, a narrower escaping-exception antichain, and a
  non-suspending callable are subtypes of the corresponding wider contracts; and
- explicit erased-concept upcasts to a concept whose required ABI is a subset.

Numeric widening and immutable closed-variant payload widening are value coercions, not subtyping.
They apply to owned values at expression joins and calls and may construct a different
representation; they never lift through a borrow, mutable place, pointer, owner, or FFI view. A
branch result is the unique least upper bound after `never` coercion and those declared owned-value
coercions. If none exists, the source must convert or wrap the arms explicitly. Storage layout never
follows merely from subtyping.

`option<T>` and `result<T,E>` are ordinary standard-library variants. `null` is a reader atom that
elaborates only under an expected `option<T>` type to `none`; without that expectation it is an
ambiguity error. Conditions require `bool`; no other value is truthy or falsy.

Integers trap on overflow in every profile. Widening within one signedness family and `f32 -> f64`
is implicit. Narrowing, float/integer conversion, and every signed/unsigned conversion require
`cast`; a constant out of range is a compile error and a runtime out-of-range cast traps. Named
wrapping, saturating, and checked operations are library operations. The required prelude's
`Integer<T>` and `NumericCast<From,To>` are intrinsic closed evidence: only the numeric types and
conversion pairs enumerated in this section satisfy them, and user implementations cannot extend
the set or turn `cast` into an arbitrary representation conversion.

Integer `/` truncates toward zero and the named `remainder` operation has the dividend's sign,
satisfying `a == (a / b) * b + remainder(a, b)` whenever the division is defined. Division or
remainder by zero traps;
dividing the minimum signed value by `-1` traps as overflow. Left shift requires a nonnegative count
strictly smaller than the left operand's width and traps when the mathematical result is not
representable. Signed right shift is arithmetic; unsigned right shift is logical. An invalid shift
count traps. Version 0.1 spells shifts as named `shift-left` and `shift-right` operations rather than
adding precedence-bearing punctuation. Named wrapping shifts reduce their count modulo the width;
ordinary shifts never do.

`f32` and `f64` are IEEE 754 binary32 and binary64 values. Basic arithmetic and square root round
once to nearest with ties to even at the declared width. Implementations MUST NOT retain excess
precision, reassociate operations, contract a multiply and add, flush subnormals, or select another
rounding mode unless the source calls an operation whose contract explicitly requests that policy.
Division by zero and finite overflow produce the corresponding infinity; invalid operations produce
one canonical quiet NaN for the result width. Input NaN payloads may be preserved by representation
operations, but ordinary arithmetic never promises payload propagation. `+0.0` and `-0.0` compare
equal. A NaN compares unequal to every value, including itself; ordered comparisons with NaN are
false, and `!=` is the negation of `==`. Consequently raw floats do not satisfy `HashKey`; maps use
an explicit total-key wrapper or policy that states its NaN and signed-zero canonicalization.

The ten punctuation operators are required-prelude bindings, not parser magic. `+`, `-`, `*`, and
`/` resolve exactly one `Add`, `Sub`, `Mul`, or `Div` evidence bundle and return that bundle's
associated `Output`; `==` and `!=` use `Equal`; ordered comparisons use `Compare`. Their required
contracts are pure, nothrow, and non-suspending, so an effectful domain operation must use a named
call rather than hide work behind punctuation. Built-in numeric evidence implements the arithmetic
rules above. There is no fallback from a missing operator binding to a similarly named function.

Integer-to-float and float-to-float `cast` round to nearest, ties to even. Float-to-integer `cast`
truncates toward zero and traps for NaN, infinity, or an out-of-range result. Constant evaluation,
the reference interpreter, verified native code, and portable ABI adapters use these same rules.
Transcendental library operations must publish their own accuracy and reproducibility contract;
they are not silently treated as correctly rounded core operators.

`string` is immutable valid UTF-8. Byte, scalar, and grapheme traversal are distinct. Integer string
indexing is absent. String/byte conversion is explicit except for a scoped zero-copy UTF-8-byte view
of a valid string.

## 6. Functions, inference, and callable contracts

Evaluation order is eager and left to right. Lexical scope is static. A private acyclic definition
may omit types determined by forward local inference. Expected types flow inward only within the
current expression; later statements do not revise earlier bindings. Public, recursive,
cycle-participating, FFI, and otherwise ambiguous definitions require explicit quantified
signatures.

CoLisp parameter forms are exactly:

```lisp
(x : T)                  ; readonly borrow (default)
(borrow-mut x : T)       ; exclusive borrow
(steal x : T)            ; ownership transfer
```

`consume-value` is an IR/Co-Forth operand-cell mode, not a CoLisp parameter keyword. A `Copy` lexical
value may be materialized into a consumed operand without invalidating its binding.

An argument to a readonly-borrow parameter is passed by value when its type is `Copy` and as a
loan otherwise. An argument to a `borrow-mut` parameter is always a loan. When the argument is a
temporary rather than a place, the calling expression owns the temporary and drops it after the call
completes. A `steal` parameter receives ownership; passing it a loan is rejected with
`F-DIAG-MOVE-FROM-BORROW`.

A callable contract contains ordered parameters and ownership modes, result, generic parameters and
bounds, capability/effect row, exception summary, suspension summary, and—where relevant—linkage and
ABI classification. Omitted private contracts are inferred. A published callable MUST explicitly
choose `pure`, concrete requests/open row tails, or `effects-infer`; it also MUST explicitly choose
`nothrow`, `throws<E...>`, or `throws-infer`, and `non-suspending`, `suspends`, or
`suspends-infer`.

Overload candidate collection and ranking are finite and deterministic. They use explicit
type/value/dispatch arguments and the already-known types of value arguments only. Import order and
an expected result type never select or break a tie. Candidates may not differ only by result type,
representation, or static-versus-erased dispatch; those choices require an explicit dispatch
argument/type application or distinct names. Erasure and recovery of a concrete type are explicit,
so a concrete argument never simultaneously matches a static and `dyn` overload.

Generalization is allowed only for variables not free in the environment when the initializer's
residual effect row is empty after local-state masking and the value captures no loan, capability,
task/fiber, mutable cell, or generative compile-time identity.

### 6.1 Static judgments

The normative checker is characterized by these judgments; an implementation may use a different
algorithm only when it produces the same accept/reject result and principal diagnostic:

```text
Σ ; Γ ; L ⊢ e : T ! ε throws X suspends s ⊣ L'
Σ ; Γ ⊢ T kind K
Σ ; Γ ⊢ ε1 ≤ ε2
Σ ; Γ ⊢ X1 ≤ X2
Σ ; Γ ⊢ evidence C<T...> unique
Σ ⊢ module M sealable as I
⊢ IR-function F certified as C
```

`Σ` is the immutable module/type/evidence environment, `Γ` is the lexical value/type environment,
and `L`/`L'` are place initialization and loan states. `ε` is the capability/state effect row, `X`
is the escaping exception set, and `s` is the suspension summary. These outputs are derived together;
no frontend may submit one as an unchecked fact.

The required compositional rules are:

- a literal has its declared type, empty row/set, non-suspending summary, and unchanged `L`;
- a readonly use creates a bounded shared loan; `borrow-mut` creates an exclusive loan; `steal`
  requires an initialized unloaned place and marks it moved in the output state;
- a call checks arguments left to right, substitutes generic/evidence bindings, unions instantiated
  rows and exception sets, joins suspension summaries, and applies the callee ownership transition;
- a sequence threads `L` and unions summaries in evaluation order;
- a branch checks each reachable arm from the same incoming state, computes the unique result
  least-upper-bound defined in section 5, and joins the least-permissive outgoing loan/initialization
  state;
- exception summaries are canonical antichains under subtyping; a handler removes an exception
  type from `X` only when its catch-pattern matrix exhaustively covers that type, while a partial
  catch retains the type because an unmatched value continues unwinding unchanged;
- a lexical state region removes `state<r>` only under the non-escape condition in section 7;
- a loop computes a finite fixed point over `L`, widening conservatively when proof fuel is reached;
  and
- a function checks its derived summaries against the declared upper bounds, then verifies all owned
  places have exactly one outgoing cleanup obligation or valid transfer.

Type/effect/evidence normalization is deterministic and fuel-bounded. The unbounded declarative
judgments define validity; reaching an implementation budget produces `resource-exhausted`, a third
compile outcome distinct from both acceptance and semantic rejection. It is never evidence that the
program is ill-typed, and increasing a budget MUST NOT turn one semantic rejection into acceptance.

Every compiler result records the individual budgets and peak use for syntax nodes, expansion
steps, CTFE steps and recursion, type/evidence normalization steps, generic instantiations, memory,
and wall-independent work units. Successful semantic results are independent of administrative
budgets and use no budget value in their identity; a cached resource-exhausted outcome records the
exact limits that produced it and is reusable only under no-larger limits. A conforming
implementation MUST support at least 1,000,000 syntax/expansion nodes,
10,000,000 CTFE or normalization steps per module, 512 nested CTFE calls, 65,536 concrete generic
instantiations per package, and 256 MiB of compiler-owned working memory per module. It may offer
larger declared limits. Wall-clock time is a watchdog/liveness bound, never the semantic fuel
counter. A program exceeding a declared limit may be retried with a larger limit without changing
its source or identity.

## 7. Effects, exceptions, suspension, and purity

An effect row is a canonical finite map of typed labels plus at most one tail row variable:

```text
{ l1(args), ..., ln(args) | e }
```

Identical labels are idempotent. Capability labels with different normalized selectors remain
distinct. Calls union rows. A caller contract is valid when the inferred row is contained by its
declared row and the instantiated request is contained by an effective grant at execution.

Capability effects are authority requirements, not authority values. Imports, evidence, paths, and
resources do not manufacture grants. Selector containment is decided over the closed selector AST
defined in section 14.

Local mutation introduces `state<r>` for a fresh lexical region. It is masked at scope exit only if
no result, owner, borrow, closure, task, exception payload, or stored value escaping the scope refers
to `r`. A computation whose only effect was masked local state may satisfy `pure`. Mutation through
shared/external state is not masked. Reading concurrently mutable shared state also introduces its
state-read label because the observation is not referentially transparent. Capability effects are
never masked this way.

Typed exceptions are control edges carrying ordinary values. A handler subtracts exactly the types
for which its ordered pattern matrix is exhaustive; unmatched values continue unwinding in their
original transfer envelope. A partial catch does not remove that type from the static escaping set.
`rethrow` has type `never` and preserves the envelope. `result<T,E>` remains ordinary data. Traps
are uncatchable protected failures and never satisfy `throws`. Cancellation is a protected scheduler
signal observed at marked safepoints and is not source-catchable.

Suspension is a separate callable contract because loans cannot cross it. Ordinary callers need no
`async`/`await` coloring; the compiler and verifier still know every possible suspension point.

`pure`, `nothrow`, and `non-suspending` are verifier-derived predicates. `pure` means the residual
effect row is empty after permitted local-state masking and the callable is referentially
transparent: its return, thrown value, semantic yield sequence, or divergence is determined only by
its explicit inputs and immutable dependencies. Reads of clocks, randomness, scheduler state,
shared mutable state, host state, or unstable identity are effects and therefore not pure. Purity is
orthogonal to exceptions and suspension. `nothrow` means the escaping exception antichain is empty.
`non-suspending` means no reachable semantic or host suspension edge exists. A pure function may
throw. Optimizations MUST preserve evaluation order, exceptions, ownership, destruction,
suspension, and externally observable effects.

A non-suspending direct call requires no continuation object, scheduler registration, journal entry,
or heap allocation merely because the language also supports resumable code. Suspension machinery
is introduced only on a reachable suspension edge.

Version 0.1 has no public `total` predicate or general termination-proof language. The compiler may
derive an internal termination certificate for intrinsics, bounded loops, structural recursion, and
other mechanically proven cases. That certificate may enable speculation or reordered pattern
dispatch only when the other required purity/failure facts also hold; it is not source syntax or a
callable interface promise.

## 8. Places, ownership, borrowing, and destruction

A place is a local or a projection path through fields and statically proven indices. The checker
tracks initialization and loans per place. Dynamic indexing conservatively overlaps the whole
aggregate unless a proof establishes disjointness.

A shared loan permits reads and forbids mutation/move/drop of overlapping places. An exclusive loan
permits mutation and forbids every other overlapping access. A reborrow suspends access through its
parent for the reborrow's lexical extent. At a control-flow join, the state is the least permissive
state valid on every incoming edge.

Loans MUST NOT escape their traced owner, cross suspension, enter durable storage, transfer to a
task, or cross FFI. A returned borrow is legal only when it traces to a finite set of input parameters
and the callable contract records that provenance set (`returns-loan<arg(i)...>`). At a control-flow
join within the callee, the returned loan origin is the set union of reachable input origins. At the
call site, the caller registers an active loan on every argument place mapped to the provenance set,
forbidding moves and exclusive access on any member of the set until the returned view reaches its
last use. Multiple-input provenance unions introduce no source lifetime parameters (`'a`). No source
lifetime parameters exist in version 0.1.

Aggregates and nominal types (records, variants) store owned data or explicit handles (`Copy`,
`Unique<T>`, `Shared<T>`, `Weak<T>`, or inline aggregates); they cannot declare borrowed reference
fields or type lifetime parameters. Long-lived graphs, cyclic structures, and UI hierarchies manage
identity and back-references through explicit weak handles (`Weak<T>`) or arena indices. When a callable
returns a composite aggregate carrying borrowed views derived from multiple inputs, the composite view's
provenance is the conservative union of all contributing inputs. To maintain disjoint borrow
independence across distinct caller places (where modifying one source place must not conflict with a
view into another), callers project each borrow through a separate accessor callable rather than a
monolithic multi-borrow return. Because loans cannot cross suspension, stackless state machine activation
records store only owned values, eliminating self-referential pointers and requiring no `Pin` wrapper.

Ownership is proven at compile time and erased. No run-time state records who owns a value, whether
a place has been moved from, or which loans are live. For every read of a place the compiler
decides, from the place's type and binding kind alone, whether it is a copy (the type is `Copy`), a
borrow (the read is an argument to a borrowing parameter or a borrowing match of a value that is
not `Copy`, or the place itself holds a loan), or otherwise a move. A binding moved on any path into
a control-flow join is moved after the join, so the compiler inserts the drop on the paths that did
not move it. That drop runs as the path ends, before anything after the join: at the end of an `if`
or `match` arm (after the arm's own binders and the scrutinee are released), and at the end of a
`try` body or handler arm. Several such drops run most recent binding first. A binding the body of a
`try` moves is also dropped when control leaves that body abruptly before the move, whether by an
exception, a protected edge, `return`, `break`, or `continue`, and it is dropped before any handler
arm runs; a handler arm therefore never sees it. A binding declared outside a loop cannot be moved
inside it. A callable's body is
checked once and means the same at every call site. The only ownership work left at run time is
running the drops the compiler placed and counting references for `Shared` owners.
[`static_check.py`](../../scripts/language/static_check.py) is the reference for these decisions on
the executable core, and the conformance check requires it to agree with the reference machine on
every read every execution vector performs.

Moves transfer the cleanup obligation and invalidate the source. A source-visible aggregate is
always either fully initialized and usable or consumed; moving a projection such as `person.name`
out of a live aggregate is rejected. An ownership-pattern destructure may atomically consume the
whole aggregate and distribute its fields into new bindings. Every owned field must be bound or
matched by a discard pattern that drops it immediately. Borrowing patterns do not consume the
aggregate. The checker and verifier still track partial initialization internally while constructing,
destructuring, and cleaning up values, but that state is never a usable source binding.

Every consuming call site is represented in semantic/editor metadata with the consumed binding and
callee parameter ordinal. A later use diagnostic names the move site and suggests `retain`, an
explicit copy, a borrow, or a reordered ownership transfer only when that operation is actually
available. Editors may render the invalidated binding, but no optional lint is responsible for the
correctness error. Ownership behavior never depends on an optional lint or editor feature.

`Copy` is explicit evidence and may be implemented only when copying creates no second cleanup
obligation and every contained value is `Copy`. `Unique<T>`, `Shared<T>`, and `Weak<T>` all move by
default. `Shared<T>` is deliberately not `Copy`: producing another strong handle requires explicit
`retain`, and producing another weak handle requires explicit `retain-weak`.
Passing a shared carrier to a stealing parameter moves that handle and invalidates the source. A
caller that must keep its handle passes explicit `retain` instead; no call-site adaptation silently
increments a count.

`borrow-mut` and `record-set!` provide exclusive local mutation. Locals are immutable by default;
`(let [mut x initial] ...)` declares a mutable CoLisp local and `(set! x value)` reassigns it.
Co-Forth local assignment uses `to x` and requires the local to have been declared `mut`. Both lower
to the same place write and `state<r>` effect.

Assignment evaluates and validates the complete right-hand expression into a temporary before
changing the destination. If evaluation throws, traps, suspends, or is cancelled, the old value
remains installed. After successful evaluation, loans that would conflict with replacement must have
ended; the old value is dropped, and the temporary is moved into the initialized destination. Drop
is nothrow and nonsuspending, so no source step can observe the destination uninitialized. A
right-hand result that still borrows from the value being replaced is rejected. `set!`, `to`,
`record-set!`, and mutable indexed assignment use this same order.

Construction registers cleanup as fields become initialized. Destructuring transfers or discharges
each field obligation before invalidating the aggregate obligation. Drops and guards share one
lexical LIFO cleanup stack. Move transfers an obligation; it never duplicates one. Drop is
non-suspending and nothrow. On exception, language trap, or cancellation, all registered cleanup
runs in reverse order. A guard failure on normal exit becomes the primary failure; during an
existing failure it is attached as suppressed diagnostic data and cannot replace the primary
failure. Fatal host faults are outside language semantics and are not promised to unwind.

A `scope` registers its guards in source order before its body runs, and each guard runs at most
once when control leaves that scope. Every exit has exactly one exit class:

| Exit class | Exits |
|---|---|
| `success` | normal completion, `break`, `continue`, `return`, and a tail call |
| `failure` | a raised exception, and the protected edges `Trap`, `Denied`, and `ResourceExhausted` |
| `cancel` | the protected edge `Cancel` |

| Guard | `success` | `failure` | `cancel` |
|---|---|---|---|
| `on-exit` | runs | runs | runs |
| `on-success` | runs | | |
| `on-failure` | | runs | runs |
| `on-cancel` | | | runs |

`on-failure` covers every exit that did not succeed, so compensation is written once. Cancellation
is not catchable, and `on-cancel` is the only way for code to react to it specifically; it can
observe the cancellation but cannot stop the unwind. A guard that does not apply is discharged
without running. Guards and drops run last registered first. A guard body has type `unit` or `never`. It is a separate control context: `break`,
`continue`, `return`, and `yield` inside a guard body cannot target anything outside it
(`F-DIAG-LOOP-TARGET`, `F-DIAG-GUARD-ESCAPE`).

When a guard fails while the exit class is `success`, that failure becomes the primary outcome and
the remaining guards of every scope being left run with exit class `failure`. When a guard fails
while the exit class is `failure` or `cancel`, the failure is appended in order to the primary
outcome's suppressed list and unwinding continues; a suppressed failure is never catchable and never
replaces a protected edge. When an expression is abandoned, its not-yet-consumed temporaries are
dropped most recent first before the guards and bindings of the enclosing construct run. A value
discarded as a non-final sequence item is dropped at that point. A thrown value is dropped when its
handler arm completes, unless `rethrow` moved it onward.

Safe cross-thread/task transfer requires `Transfer<T>` evidence; shared borrowing across such a
boundary requires `Share<T>`. Interior mutation requires an explicit synchronization abstraction.
Unsafe carrier implementations are trusted only under a versioned safety contract; safe code may
rely on no invariant absent from that contract.

Closure capture is part of the closure's checked type. A closure proven not to escape or suspend may
infer scoped shared or exclusive borrows from its uses. A stored, returned, deferred, dynamically
erased, or otherwise escaping closure MUST use the move-capture form or an exact capture list.
Capture entries choose shared borrow, exclusive borrow, explicit `Copy`, explicit strong retain,
weakening, or ownership transfer. Move capture transfers every non-`Copy` binding, including
`Shared` and `Weak` handles, and invalidates the source; retaining a shareable carrier requires the
`retain` entry. It never silently promotes a borrow into an owner. No borrowed capture crosses suspension, checkpoint,
worker transfer, FFI, or the lifetime of its traced owner. Both frontends lower their capture syntax
to the same ordered `CaptureSpec`.

### 8.1 Shared and weak lifecycle

A shared control block has atomic strong and weak counts. The weak count includes one implicit weak
while strong is nonzero. Retain increments strong with relaxed ordering. A strong or weak release is
a release decrement; only the thread observing the transition to zero performs an acquire fence.
Last-strong release then drops `T` and releases the implicit weak. Last-weak release then deallocates
the control block. `upgrade` uses a compare/exchange loop that increments a nonzero strong count,
with acquire ordering on success and relaxed ordering on a failure that observes zero. Failed races
retry with the newly observed count. Counts cannot wrap; an attempted overflow traps before changing
the count.

### 8.2 Ownership-carrier and pinning laws

The compiler lifecycle kernel knows definite initialization, moves, loans, last use, copyability,
movability, and exactly-once cleanup. `Unique`, `Shared`, allocators, and control blocks are library
implementations rather than compiler-name special cases. The required ownership concepts have these
semantic contracts:

```text
Lifecycle.drop(steal Self) -> unit
Owner<T>.borrow(&Self) -> scoped &T
ShareableOwner<T>.retain(&Self) -> Self
StableAddressOwner<T>.stable-borrow(&Self) -> scoped stable &T
PinnableOwner<T>.pin(steal Self) -> Self::Pinned
TryPinnableOwner<T>.try-pin(steal Self)
  -> result<Self::Pinned, PinFailure<Self>>
TryUniqueRecoverable<T>.try-into-unique(steal Self)
  -> result<Unique<T>, Self>
```

`Pinned` MUST implement `StableAddressOwner<T>`. `Owner<T>` keeps its pointee valid and at one
address during each active borrow but may relocate it between loans. A stable-address owner keeps
the address fixed for its complete live storage generation. Pinning requires an unloaned owner and
grants address stability, not a longer lifetime or host authority. Infallible `pin` cannot allocate,
reserve, throw, or suspend; fallible `try-pin` returns the original live owner on failure.
`PinFailure<Self>` is an affine closed failure value that owns exactly one live `Self`; consuming
`recover-pin-owner` returns it without allocation or failure. It is never a diagnostic-only marker.

Copy and drop hooks are bounded, deterministic, non-suspending, and nothrow. They may perform only
their declared memory-local lifecycle effects and cannot acquire ambient authority or hide
externally fallible work. Close, flush, commit, and protocol shutdown are explicit operations or
guards rather than destructors. Lifecycle behavior is classified as `trivial-drop`, `local-drop`,
or `ordered-drop`; composition takes the greatest class of all possibly live fields and the custom
hook. Ordered drop is an optimization sequencing barrier.

`Lifecycle` is the sole cleanup concept; there is no separate `Drop` concept. Trivial and structural
lifecycle implementations are compiler-derived. A custom lifecycle hook is admitted only when the
verifier derives its internal termination certificate; user assertion cannot manufacture that
certificate. `Copy` is likewise compiler-derived from fields or admitted only by a trusted unsafe
implementation whose audit obligation proves that copying creates no second cleanup obligation.

`Transfer<T>` and `Share<T>` are derived structurally by the compiler when every reachable field has
the corresponding evidence and no unsafe negative condition applies. A manual implementation is
unsafe and must prove the versioned carrier/synchronization contract. `Share<T>` never follows from
reference counting alone; reachable interior mutation must provide a synchronization abstraction.

## 9. Records, variants, patterns, and layout

`record` defines one nominal product. Storage placement and owner carrier are independent.

A record or variant may be declared inside a function body. Such a **local type** is in scope only
in that body, yet a value of it may be returned, so a caller can hold, pass, and use a value whose
type it cannot name (a Voldemort type). The caller binds it with an inferred type and reaches the
type itself only through inference or reflection on the value or on the function's result. A
function that returns a local type has an inferred result type, since the type cannot be written in
its own signature. The type's identity is the enclosing function's identity, its generic arguments,
and the local name, so each instantiation of a generic function has its own. The sealed interface
of an exported function carries the local type under that identity with its public members. The
executable core does not implement local types yet; the open points are listed in DESIGN_REVIEW
finding 80. Fields and
members are private by default and may be `pkg` or `pub`. Inherent behavior is declared in
`implementation T` using `constructor`, `get`, `set`, and `operation`. A type declaring any
constructor permits raw literal construction only inside its constructors.

`record-set` is functional in the sense that it returns the replacement record rather than mutating
through a borrow; it consumes a non-`Copy` input record, while a `Copy` input may be materialized
without invalidating its binding. `record-set!` writes through an exclusive borrow and returns `unit`.
Both take a compile-time field keyword (`:field`), never a runtime string or ambient lexical name.
For example, CoLisp uses `(record-set! self :balance value)` and Co-Forth uses
`self :balance value record-set!`; both normalize to the same checked field selector. Ordinary
member reads are `(. value field)` and `value .field`. CoLisp member assignment is
`(set! (. value field) replacement)`; Co-Forth uses `value replacement .field!`. A member mutation
selects a writable field or `set` property and otherwise fails during elaboration.

`record-set` and `record-set!` are checked intrinsics: elaboration must prove that `R` is the named
record containing field `F`, that `V` is accepted by that field, and that visibility and mutability
permit the operation. They are not unconstrained generic functions and cannot be implemented or
overridden by user code.
For a stored field, the replacement order in section 8 applies. For a `set` property, the receiver
and complete argument are evaluated before the accessor is called, but the accessor is an ordinary
checked callable: its declared effects/exceptions govern, and no implicit rollback promise is made
for mutations it performs before a throw or protected failure.
Member resolution is field, property, inherent operation, then concept operation, and last a free
function: when none of the first four matches, `value.name(arguments)` is the call
`name(value, arguments)` under ordinary overload resolution (uniform call syntax). A concept
operation therefore always wins over a free function of the same name. A property is read without
an argument list. Multiple concept operations of the same unqualified name require qualification.
The C-like reader of 0.1 does not yet apply this rule, which needs type information the reader
does not have; it is a rule of name resolution.

Variants are closed nominal sums. Payload is unit, tuple, or record. Ordinary `match` is exhaustive.
Catch matching may be partial. Shadowed or broad-before-specific overlapping patterns are errors;
incomparable overlapping patterns are ambiguous.

Matching a borrowed aggregate borrows bindings from the selected payload. An ownership-pattern
match consumes the whole aggregate and transfers or immediately drops every field; it never leaves
a partially moved aggregate binding. An unmatched catch has not selected an ownership pattern and
continues unwinding with the complete original exception value.

**Taking a record apart.** Dropping a value has two steps. First its type's custom lifecycle drop
hook runs, if it has one, with an exclusive borrow of the value; the hook uses the fields and does
not take them. Then every field is dropped, in reverse declaration order, each by its own type's
lifecycle. The second step is compiler-derived and happens whether or not a hook exists, so an owned
field the hook never mentions is still released.

**Failure during construction.** A value of a record type exists from the moment its record
literal finishes and not before; a constructor is an ordinary function that contains one. This
fixes every case:

- If evaluating a field's initializer raises or traps, the fields already initialized are dropped
  in reverse order of initialization, each by its own lifecycle, and the type's drop hook does not
  run, because no value of the type ever existed. The failure then propagates unchanged.
- If a constructor fails before its record literal is reached, only its own locals exist and they
  are dropped as in any function.
- If a constructor fails after its record literal finished, the value is complete and is dropped
  like any other local: hook first, then fields.

No partially constructed value is ever observable, by the caller, by a handler, or by a drop hook.
A caller that sees a constructor fail has nothing to clean up.

**Failure during destruction.** A drop hook cannot raise: hooks are `nothrow` and non-suspending,
which the checker enforces, so "an exception escaping a destructor" does not arise. Work that can
fail (close, flush, commit) is an explicit operation or a guard, where the failure has a handler.
A hook can still trap, for example on integer overflow. In that case the value's fields are still
dropped, in the usual order, and the trap then follows the rule for a failing cleanup in section 8:
it becomes the primary outcome if the exit in progress was a success, and is recorded as suppressed
under the outcome already in flight otherwise. Dropping never stops halfway.

An ownership pattern on a record obeys these rules:

1. **Visibility.** A pattern may name only the fields and `get` properties visible where the
   pattern is written: public ones from outside the type's package scope, all of them inside it. A
   property named in a pattern is called, in the order the pattern lists its parts, before anything
   is dropped.
2. **Unnamed fields.** A pattern that does not name every visible field must end with the rest
   marker `...`. Without it, an unnamed visible field is an error (`F-DIAG-PATTERN-INCOMPLETE`).
   Fields not visible at the pattern are always covered by `...`, which is then required.
3. **The remainder is dropped at once.** After the named parts are bound, what is left of the value
   is dropped before the next expression is evaluated.
4. **A type without a drop hook** gives up each named owned field by move. The fields not named are
   dropped individually, in reverse declaration order.
5. **A type with a drop hook** never has an owned field moved out by a pattern
   (`F-DIAG-MOVE-FROM-HOOKED`). A pattern on such a type may bind copies of fields whose type is
   `Copy` and the results of properties. The value is then dropped whole, hook first and fields
   after, so the hook always runs over a complete value. Whether a hook's body happens to use a
   given field does not change this: the rule depends only on the type's public surface.

A type with a drop hook that wants to hand out its owned parts provides an ordinary consuming
operation returning a plain record, and the caller applies a pattern to that result. There is no
user-defined hook for taking a record apart. A refutable pattern over a value whose shape is known
only at run time, such as a JSON-like value, is a separate feature (a user-defined matcher that
returns an option of the parts) and is not part of 0.1.

`repr(native)` is the default. `repr(C)` follows one declared target C ABI and is not portable wire
layout. `repr(stable N)` fixes scalar encoding, endianness, field order, alignment, padding,
discriminants, and validity; changing any requires a new version. References, owners, capabilities,
resources, and evidence pointers require an explicitly specified stable handle representation.

## 10. Concepts, evidence, and generics

A concept declares associated types/values and operation contracts. Satisfaction requires one
explicit implementation; matching names never imply conformance. An implementation maps every
requirement to a callable or adapter checked against receiver ownership, parameters, result,
effects, exceptions, and suspension.

An abstract requirement has no body from which an `*-infer` marker could be computed, so those
markers are forbidden on requirements. It must state fixed axes or bind explicit `effect e`,
`exceptions X`, and suspension parameters on the concept and use those variables in the contract.
The evidence type carries those parameters, which lets clients check once against a bound without
rediscovering implementation effects after instantiation.

Coherence key is `(concept identity and arguments, outermost implemented type family)`. An
implementation is legal only in the module owning the concept or the outermost nominal implemented
type. This orphan rule is checked while sealing that module. At most one implementation exists for
the whole type family; specialization through overlapping implementations is absent.

Conditional implementations may bound their parameters, but two differently bounded
implementations for the same family remain forbidden. A wrapper/newtype is the explicit route to a
second behavior.

Associated projection is `T::Name`. It requires unique evidence and normalizes with cycle detection
and bounded fuel. A recursive normalization cycle is a compile error.

Static evidence type-checks a generic body once against bounds. Its required baseline is a certified
parametric/shared IR body with statically selected evidence; a compiler MAY lazily specialize a
bounded set of hot or representation-dependent type/value/evidence tuples. It MUST NOT require one
native body per tuple when the shared representation and operations suffice, and specialization
limits and decisions do not change program meaning or interface identity. Dynamic evidence uses an
explicit `dyn C<...>` view and a fixed ABI table. Source-level erasure is never implicit.

A `stable-evidence` concept assigns permanent operation keys and declares `evidence-version N`.
Removed keys remain retired. The concept's sealed revision owns compatibility; implementations
automatically record the revision and have no source-level `dynamic-evidence-version`.

A concept may declare a named `axiom` whose expression is pure, `comptime`, and depends only on the
concept's type/value parameters, associated types/values, and other compile-time constants. Every
matching implementation evaluates every axiom exactly once after its mappings and bounds are known.
False or ill-typed evaluation rejects that implementation with the axiom's source origin; resource
exhaustion yields the distinct compile outcome defined in section 6. No outcome silently removes a
resolution candidate. Axioms cannot quantify over or
claim properties of arbitrary runtime instances. Undecidable algebraic laws such as `symmetric` and
`commutative` remain explicit trusted laws, not fake compile-time proofs.

`trusted-law` marks a concept whose safe algorithms depend on runtime laws that cannot be proved by
the language. Implementing such a concept is an unsafe, reviewable assertion. `Equal` promises
reflexivity, symmetry, and transitivity for values in its documented domain; `HashKey` additionally
promises `equal(a,b) => hash(a) == hash(b)`. Maps may rely on those laws, so a safe implementation
cannot merely provide two callables with matching types.

An explicit `dyn C<...>` view is legal only when `C` is erasable. Erasability requires every
associated type/value needed by a client to be bound in the view and every exposed operation to have
one finite, layout-independent ABI slot. A generic operation, unbound `Self` in an exposed result,
compile-time-layout-dependent operation, or unbound associated output is not dyn-compatible unless
the concept explicitly reifies it through a fixed ABI type. The owned erased view carries the
concrete owner/lifecycle evidence; a borrowed view carries only its bounded borrow and immutable
evidence table. Static-to-dynamic erasure, dynamic checked recovery, and evidence-subset upcasts are
explicit operations.

Generic headers may bind types, `value N : T`, type/value/parameter packs, and `infer` outputs.
Value expressions in type positions execute by bounded pure CTFE. Bounds are checked before body
generation; candidate selection never compiles arbitrary bodies to see which succeeds.

## 11. CTFE, reflection, and hygienic syntax

CTFE executes ordinary typed Finch under deterministic fuel, recursion, allocation, expansion, and
instantiation budgets. It cannot perform runtime host capability effects. Curated compiler queries
carry `! comptime` and are discharged only by full constant folding or `mixin` of a syntax result.

`syntax` values carry source origins, expansion ancestry, phase, scope sets, and eventual binding
identity. Datum values do not. Identifier resolution chooses the unique most-specific binding scope
set; no match is unbound and incomparable maximal matches are ambiguous.

A transformation call is created only after the callee resolves statically to a callable with a
`syntax` parameter. The corresponding source argument remains unevaluated retained syntax. A
dynamically selected callable cannot capture syntax. Co-Forth passes retained code with
`syntax[ ... ]`; the obsolete `macro:` registration form is not conforming.

Required compile-time operations include:

```text
datum->syntax(context, datum) -> syntax
syntax->datum(syntax) -> datum
fresh-name(context, hint) -> syntax ! comptime
type->syntax(type, context) -> syntax ! comptime
function-spec-of(identifier-syntax) -> FunctionSpec ! comptime
members-of(type) -> list<MemberSpec> ! comptime
fields-of(type, options) -> list<FieldSpec> ! comptime
modules-of(program, options) -> list<ModuleSpec> ! comptime
include-str(path) -> string ! comptime
include-bytes(path) -> bytes ! comptime
```

`datum->syntax` accepts keyword datum as a distinct kind. `type->syntax` preserves resolved type
identity even when no original spelling exists. Reflection results are immutable and versioned.

`ct-range(start,end,step)` produces a finite half-open integer range after constant evaluation.
Zero step or overflow is an error. `ct-foreach` executes at compile time and may use CTFE-permitted
effects, including maskable local state, but no runtime/host capability. Runtime code generation
returns syntax and enters the module only through `mixin`.

Generated declarations receive the invocation site's module membership and visibility, retain both
definition and use origins, and pass normal resolution, coherence, ownership, effect, and verifier
checks. They cannot replace an already published declaration.

`@Name form` is reader sugar for `(mixin (Name form))`, applied to the next form only. Stacking is
left-to-right syntactically and therefore innermost-first semantically: `@A @B F` becomes
`(mixin (A (mixin (B F))))`. `Name` resolves normally and MUST return `syntax`; otherwise ordinary call
typing rejects the expansion.

In the C-like frontend, syntax quasiquotation is expressed using `syntax { ... }`, with `$ident` and
`$(expr)` unquoting/interpolation (avoiding collision with generic type templates). Macro invocation
at the call site uses the shorthand `name!(args...)`, which is reader sugar for
`mixin(name(syntax { arg1 }, ...))`. Arguments to a macro call are passed unevaluated as `syntax`
objects, and the returned `syntax` AST is spliced in-place into the caller's AST. This is
syntactically distinct from a generic function call with explicit template arguments, written
`callee!(T)(runtime_args...)`, where `!(...)` supplies compile-time type/value arguments and the
trailing `(...)` supplies runtime call arguments. First-class `syntax` reflection and pattern matching
allow CTFE functions to inspect AST structure, type information, and literals to perform compile-time
optimizations such as dead-branch elision before splicing.

Declaration metadata is structured syntax produced and reflected by these transformations, not a
second magic attribute mechanism. Core attributes are namespaced and may be imported or renamed like
other compile-time bindings.

## 12. Control, cleanup, tail calls, and resumability

Tail position is the final expression of a function, lambda, or implicit submission entry callable.
It propagates through the selected arm of `if` and `match`, the last item of `begin`, and the body
of `let` and `scope`, in each case after required cleanup. The body of `try`, a handler arm, and the
body of a generator are not in tail position: a `try` body's handlers must still observe the call, a
handler arm still owes the release of the exception it caught, and a generator's frame belongs to
its handle. The operand of `return` is in tail position unless the `return` is written inside a `try`
body, a handler arm, or a generator body of the same callable. Every call to a function or
closure in tail position MUST be a proper tail call, including self, mutual, and indirect calls.
Cleanup executes before transfer and MUST NOT retain the current frame. Conforming implementations
support an unbounded number of active tail calls with bounded call-frame space.

A tail call cannot pass a loan of a place owned by the frame it discards
(`F-DIAG-LOAN-ESCAPES-FRAME`). A temporary passed to a borrowing parameter in a tail call has no
remaining caller to own it, so the callee's frame adopts it and drops it when that frame exits.
A loan a callable received through a borrowing parameter is not a loan of its own frame, so it may
pass that loan on in a tail call whether or not its frame adopted the value behind it: an adopted
value whose loan the tail call passes on (as an argument, inside a closure that captured the
loan, or as the closure being called) is adopted by the new frame instead, and every other
adopted value is dropped, most recent first, before the callee is entered. A tail call through a closure the frame owns (a local, an owning parameter, or a temporary) moves
that closure into the new frame, which adopts it; a closure the frame was only lent stays with its
owner. A closure that borrows a place owned by the discarded frame cannot be passed in a tail call,
by the first rule. The adopted values of a frame are bounded by its parameters and the closure it
runs, and a callable's body means the same whether its caller kept a value or handed it over.

`return value` transfers from the nearest function, lambda, or fiber body after running cleanup for
every exited scope. The implicit entry callable of a submission is not a `return` target
(`F-DIAG-RETURN-TARGET`). In CoLisp it is `(return value)`; in Co-Forth the result is already on the stack
before `return`. `break` and `continue` target the nearest lexical loop, run cleanup for scopes exited
between their site and that loop, and respectively leave the loop with `unit` or begin its next
condition check. Using any of these outside its required lexical target is a compile error. Cleanup
failure and protected cancellation follow the same primary/suppressed ordering as ordinary exits;
none of these forms may bypass a guard or live ownership obligation.

**Generators.** A function or lambda is a generator exactly when `yield` appears in its own body,
not inside a callable nested in it. No annotation marks it. A call to a generator evaluates its
arguments, moves them into a new **fiber**, and returns that fiber's handle; none of the body runs.
The same holds inside another generator: the call makes a handle and forwards nothing, so passing
an inner generator's items on is written out. A generator cannot be given a borrowed argument of a
type that is not `Copy`, because the fiber outlives the call (`F-DIAG-TRANSFER-REQUIRES-OWNED`). A
`yield` outside a function or lambda body, or inside a guard body, is rejected
(`F-DIAG-YIELD-TARGET`).

A fiber is a single-pass range with a one-item buffer. Its operations borrow the handle exclusively
and never consume it:

| Operation | Runs the body? | Result |
|---|---|---|
| `empty?` | only to prime | whether the fiber has finished |
| `front` | only to prime | the buffered item: a copy when the item type is `Copy`, otherwise a loan |
| `pop-front` | yes, once | `unit`; the buffered item is dropped and the fiber advances |
| `reply(args...)` | yes, once | the new buffered item; the old one is dropped and the fiber advances |
| `start` | to prime, now | the handle itself |

**Priming** is the one advance that makes the first item available: it runs the body to its first
`yield`. `start` primes at once; otherwise the first `empty?`, `front`, `pop-front`, or `reply`
primes before doing its own work, which is the single case where `empty?` or `front` runs code. After
that the body runs only inside `pop-front` and `reply`, exactly once each.

`yield value` hands `value` to the handle as the buffered item and suspends. The handle owns the
item until it is popped; a yielded value is never a loan. When the fiber is next advanced the `yield`
expression evaluates to `option` of the generator's own parameters: `none` after `pop-front`, and
`some(args)` after `reply(args)`. With one parameter that is `option<T>`; with several it is an
option of their tuple; with none it is an option of the empty tuple and only tells the body which
operation advanced it. `reply` therefore has the generator's signature: it takes the generator's
parameters and returns its item type. Every `yield` in one body yields the same type, and the
declared result is either that type or `unit` (`F-DIAG-YIELD-TYPE`). A returned value of the item
type is the fiber's last item; a generator that returns `unit` adds none. `front` on a finished
fiber, `pop-front` on one, and `reply` when no `yield` is waiting or when the advance ends the fiber
without a last item raise the exception value `RangeEmpty`. It is an ordinary exception, raised in
the reader and caught by its handlers; it is not a trap, and `empty?` is how a reader avoids it.

A failure raised by the body is raised in whichever operation was advancing it, inside that
caller's handlers. Under `start` a failure during priming is not raised there: it is stored in the
handle and raised by the next operation on it, so a failure is only ever raised at a read.

Dropping a handle ends a fiber nobody can advance again, at the drop and in the dropping
execution's own sequence. A dormant fiber drops its arguments. A suspended fiber drops its buffered
item and is then unwound from its `yield` with exit class cancel: its guards and drops run before
the dropping execution continues. Cleanup that would itself suspend during such a drop is not yet
specified. `(return value)` in a generator is never a tail call, since the frame belongs to the
handle.

A fiber, stream, or suspending task frame is lowered as a **stackless state machine activation
record**. The compiler statically computes the minimal union of live bindings and captures across all
suspension points; no private execution stack is allocated. Advancing a fiber (`pop-front`, `reply`)
or driving a task executes directly on the caller's / driving thread's existing execution stack.
The activation record is an owned resource held exclusively by the handle, allocated via the active
allocator, and destroyed deterministically upon completion or cancellation without a garbage
collector. Loans MUST NOT cross suspension points (section 8), ensuring every value preserved across a
yield or suspension edge is an owned value and preventing dangling stack references without runtime
pinning.

`option<T>` has the cases `some(T)` and `none`. `some` is a keyword only in type position
(`some Concept`), so it is free to name the case.

`task<T,X>` exposes a
terminal `join` returning `T` or raising `X`. `stream<T,X>` exposes repeated `next`, returning
`stream-step<T,X>` or raising `X`; `done` is its consumed successful terminal result and `item`
carries both the item and sole successor handle. These are policy
wrappers over resumable execution, not interchangeable handles. Spawn/constructor operations infer
`X` from the producer and seal it into the returned handle type; widening to a declared super-
antichain is explicit and exception erasure is absent.

Task, stream, and fiber constructors are invariant in their payload, resume, result, and exception
arguments. `ExceptionSubset<From,To>` is intrinsic closed evidence over canonical antichains;
`widen-task-errors`, `widen-stream-errors`, and `widen-fiber-errors` consume a handle and change only
that static exception argument. They generate no runtime work. There is no implicit variance or
exception-erasing handle conversion.

Semantic yield is distinct from `Await`: yield is chosen by source code and observed by the
fiber's reader, while `Await` parks on a typed external effect and is invisible to that reader.

`spawn` consumes a zero-argument callable and its already-materialized capture environment. It first
reserves one bounded scheduler slot and fresh child execution identity, then performs one atomic
linearization that moves the callable into the child, publishes the sole `task<T,X>` handle to the
parent, and makes the child runnable. The child cannot run before that point and may run immediately
after it, before the parent's next instruction. Failure before linearization creates no child and
leaves the callable under the parent's cleanup obligation; capacity/allocation failure is protected
`ResourceExhausted`, consistent with `spawn` being `nothrow | non-suspending`. Cancellation at the
linearization point observes exactly one side: either no child, or a published child owned by the
returned handle/parent cleanup stack.

The child's effective host grant is the intersection of the parent's live grant at linearization,
the callable's declared capability row, and host child policy, which is the set of grants the host
allows any child of this execution to hold. The executable core has no effect rows, so its reference
intersects the first and the last. It is never ambient authority and
cannot widen through a captured root, resource, module, or evidence value. Every external dispatch
still rechecks the child's live grant and generation. The `effects<e>` axis on `spawn` therefore
charges the caller for effects the concurrent child may perform; it does not eagerly execute or
journal those effects during construction. Creating a nondurable local task performs work linear in
the moved capture entries plus bounded scheduler bookkeeping and MUST NOT serialize the closure,
hash portable messages, checkpoint, execute the body, or allocate a continuation for the parent.

Because a child may run at any point after linearization, the interleaving of a parent's and a
child's observable transitions is not fixed by the language. The task execution vectors therefore
state the one admissible schedule they use (a child first runs when its handle is joined and then
runs to its terminal), and an implementation reproduces them under a scheduler configured to that
schedule. What every schedule must preserve is fixed: no child transition precedes the `spawn`
linearization, a scheduler-capacity failure creates no child and leaves the callable's cleanup with
the parent, a child's failure or protected edge reaches the joiner only after the child's own
cleanup, and a child never holds a grant its parent or host child policy lacks. A grant the host
revokes is rechecked at the next dispatch, in the root execution and in every child alike: a
request already accepted completes, and the next one is `Denied`. A callable that
captures a loan cannot be spawned (`F-DIAG-TRANSFER-REQUIRES-OWNED`).

More generally, every task/stream/fiber type argument and every captured value must satisfy the
structural owned-no-loan admissibility judgment; values transferred to another execution also need
`Transfer`. References, scoped views, and aggregates containing them fail that judgment, so types
such as `task<&T,X>` are ill-formed rather than merely difficult to spawn.

Trap, cancellation, authorization denial, and resource exhaustion remain protected terminal
outcomes rather than members of `X`. Their propagation runs cleanup and consumes the handle but
cannot be caught by pretending to add them to `throws<...>`. A combinator computes its result
exception argument by canonical union of its inputs and its own callback contracts. Thus
`join-all`, `race-and-reap`, buffering, mapping, and stream adaptation never hide a producer failure.

Cancellation is idempotent and terminal. A handle has exactly one terminal transition. Unfinished
owned handles must be joined, cancelled, transferred to a scheduler owner, or dropped under an
explicit policy. Replay identities and tie-breaking are stable.

Programmer-visible task, generator, buffered-producer, coroutine, and green-thread handles are
distinct affine policy wrappers over the same private resumable record. Scheduler parking, fuel
preemption, and host waits are not semantic yields. Every advance consumes the prior direct handle
and returns either the sole successor or one terminal outcome. Dropping an unfinished handle
atomically transfers it to the bounded scheduler reaper in cancellation-requested state; explicit
cancel consumes the handle and waits through cleanup. No handle drop abandons a live cleanup stack.

`join-all` waits for every consumed child and reports results in input order. If several children
fail, the lowest input position is primary and later failures are ordered suppressed diagnostics.
`race-and-reap` selects the first journaled terminal outcome, then cancels and reaps every loser before
returning or propagating. `select-complete` returns `task-selection<T,X>`, containing the selected
input index, its `task-outcome<T,X>` completion as data, and one `remaining-tasks<T,X>` linear composite
owner for all remaining handles. It is therefore `nothrow`; the caller may explicitly use
`result-or-throw` only after deciding what to do with the remainder. A protected selected outcome
cancels and reaps the remainder before propagating and never fabricates a catchable `X`.
Simultaneous readiness uses a persisted tie-break and rotating
fairness cursor; no general scheduling fairness guarantee is otherwise made.

The blocking name is intentional: `race-and-reap` includes loser cleanup latency. Code that needs
the winner immediately uses `select-complete` and owns the returned remainder explicitly. A
cancellation safepoint is required at backward branches and potentially unbounded library loops,
but an optimizer may count down, hoist, or coalesce polls when it proves a finite maximum instruction
distance to the next observation; the response bound is part of the compiled function certificate.

Copyability, shareability, and checkpointability are independent. A value crossing a checkpoint,
durable task, replay, process, or persistent VM revision requires `Checkpointable` evidence defining
a versioned logical encoding and reconstruction contract. Live loans, raw/native pointers,
execution-local unique resources, in-flight unacknowledged foreign calls without replay metadata,
and owners lacking such evidence prevent checkpointing. `Shared<T>` is checkpointable only when its
carrier and pointee graph are; restoration creates new owners and promises neither address nor
reference-count identity. Durable composite policies persist child identities/generations, bounded
queues, pending resumes, terminal observations, fairness state, cancellation phase, and delivery
acknowledgements before any child resumes after restart.

Ranges use one associated `Item`, not a free element parameter. Base operations are `empty?`,
`front`, and `pop-front`. `Length<Self>` supplies a runtime `length`; `KnownLength<Self,N>` refines it
with a compile-time integer value. `RandomAccess<Self>` supplies integer-indexed `get`,
`MutableAccess<Self>` supplies integer-indexed `get-mut`, `Contiguous<Self>` licenses a scoped slice,
and `Growable<Self>` supplies `push`. Every refinement inherits `Range<Self>` and uses its associated
`Item`; none introduces a second free element type. An effectful or suspending source adapts through
a concrete buffering wrapper; effects belong to operations, not the concept itself. `foreach` is
hygienic library syntax over these operations, not a compiler-only path.

`Range` is a cursor abstraction: `empty?`, `front`, and `pop-front` are worst-case O(1) and allocate
nothing; a container whose front removal is linear exposes a separate cursor value rather than
claiming Range for the container itself. `Length.length` and `RandomAccess.get` are worst-case O(1);
`Growable.push` is amortized O(1) and documents any stronger invalidation rule. `KnownLength<Self,N>`
is trusted evidence that every valid value has runtime length `N`. `Contiguous` is trusted unsafe
evidence of one bounds-checked, properly aligned element region whose returned slice traces to the
input owner. Implementing either refinement without those laws is rejected at the safe boundary.

`Freezable<Self>` names one associated immutable `Output`. `freeze` consumes a uniquely owned
builder, may reuse its allocation, creates no second owner, and performs no hidden sharing; resource
exhaustion is a protected outcome. It is not a generic wrapper around arbitrary `T`, and types that
cannot state a concrete immutable output simply do not implement the concept.

### 12.1 Syntax-neutral execution transitions

The reference execution machine repeatedly produces exactly one transition:

```text
Continue(state)                 continue within the current bounded VM slice
Emit(event, state)              record one structured external event, then continue
Await(request, resume-state)    transfer a typed request to the host/scheduler
Raise(value, provenance)        run cleanup and enter a matching handler or fail terminally
Complete(values, journal)       commit the successful transaction
Fail(diagnostic)                discard uncommitted VM-local mutation
```

Traps, cancellation, authorization outcomes, and resource limits use protected terminal/control
edges rather than catchable `Raise` values. `Emit` and `Await` are journaled before exposure;
resumption is correlated by execution and sequence identity and never reparses or resubmits source.
Both frontends' core forms elaborate to this machine through the same semantic-construction nodes.

The canonical per-form rule programs are [`transitions.json`](semantics/transitions.json). Each rule
names its form, its completion (`value`, `none`, `never`, or `terminal`), whether it is activated by
evaluating a node or by the machine, its instruction program, and the branches it can take. The
same file states the unwinding discipline shared by all rules. The rules are shared semantic policy
and never frontend policy.

[`reference_machine.py`](../../scripts/language/reference_machine.py) is the reference
interpretation: it executes a program only by running those rule programs, so an edit to a rule
program changes observable behavior. It models exactly the observable contract: the ordered
transition trace, the terminal outcome, the effect journal, the drop sequence, host-message
acceptance, and peak live frame count. It is not an implementation strategy and imposes none of its
own data structures.

[`execution-vectors.json`](fixtures/execution-vectors.json) is the conformance oracle for the
dynamic semantics. Each vector gives one program in every spelling, the normalized AST every reader
must construct, the semantic digest every event stream must share, the scripted host context, and
the complete expected trace, terminal, and final state. A conforming implementation, run on a
vector's program with the vector's host script, MUST produce the same terminal and the same
externally observable sequence: every `Emit`, `Await`, `Raise`, terminal transition, journal entry,
and nontrivial drop, in order. `Continue` entries describe reference steps; an implementation need
not materialize them, but it MUST NOT reorder the observable entries relative to one another.

[`transition-coverage.json`](semantics/transition-coverage.json) records, per rule, whether the
vectors run every instruction of its program and take every branch it declares. A rule that is not
fully covered is pending: normative intent, but not an executable conformance oracle.
[`static-rejections.json`](fixtures/static-rejections.json) lists programs that MUST be rejected
before execution, each with its stable diagnostic code.

Resolution between semantic construction and execution is shared and syntax-neutral: top-level
function declarations are hoisted (the 0.1 executable core rejects a function or variant
declaration anywhere but among the items of a submission or module, `F-DIAG-NESTED-DECLARATION`;
local type declarations are an intended feature, described in section 9, and that code is the
core's limit, not a rule of the language), each call is classified by what its callee names (intrinsic,
function, generator, closure, host effect, host await, host event, or range operation on a generator), calls in tail position are
marked, and a closure's capture list is completed from its capture default. A function named in
value position is a callable with no captures over that function's definition. A call whose callee
names a variant case constructs that case; its argument count must equal the case's payload
(`F-DIAG-CONSTRUCTOR-ARITY`). A nullary case is constructed by `(Case)` in CoLisp, by `Case()` in the
C-like syntax, and by the word `Case` in Co-Forth. In a pattern a nullary case is written `(Case)`,
`Case()`, and `Case( )` respectively; a bare name in pattern position is always a binder in every
syntax, never a case, so it matches anything.

Reading a member evaluates its target as a borrow when the target is a place. A `Copy` field is
copied out; when the target was a temporary, the rest of the aggregate is dropped immediately. A
field that is not `Copy` is read as a loan, which requires the target to be a place: moving one field
out of an aggregate is rejected (`F-DIAG-PARTIAL-MOVE`), and consuming a whole aggregate is done by
an ownership match.

## 13. Typed stack IR and verification

IR is versioned and contains typed constants, stack/local/capture operations, calls, branches,
records/variants, ownership operations, handlers/cleanup, effect requests, suspension, returns, and
source origins. Each executable function has either a concrete signature or a verified parametric
signature plus its bound evidence environment. Representation-dependent operations require
specialization before executable verification; representation-independent parametric IR is verified
once and may execute through its certified shared calling convention.

The verifier independently derives and checks:

- stack height/type at every edge and compatible merge rows;
- initialized places, moves, loans, reborrows, and exact cleanup ownership;
- call signatures, evidence identities, associated projections, and ABI classes;
- capability/effect containment, exception edges, suspension points, and predicates;
- handler/cleanup structure and proper tail-call placement;
- source/module/vocabulary identities and version compatibility; and
- that no instruction forges a capability, resource, phase certificate, or verified token.

The instruction set of IR version 6 for the executable core is [`ir.json`](semantics/ir.json). It
is the implemented version 5 plus region tables for handlers and cleanup, explicit lifecycle drops,
slot references for exclusive borrows, and frame-replacing tail calls. A function is a list of basic
blocks; each block names the innermost region that covers it, and a region is a `catch`, `cleanup`,
or `suppress` entry with a handler block, a parent, and the operand depth to restore. Regions cost
nothing on a path that does not signal.

Ownership does not exist in the IR. The lowering holds every value that needs a drop in a local slot
under a cleanup region, ends that region at the point the static pass decided the value moves, and
emits the drop inline on each normal exit, so the executor only runs `drop_value` where it is told
to. An exclusive borrow is a reference to a slot; a readonly borrow is the value itself. A frame
keeps three small lists that are empty unless used: signals being unwound through it, exceptions
whose catch arm is running, and values a tail call handed it to drop.

[`ir_lower.py`](../../scripts/language/ir_lower.py) is the reference lowering,
[`ir_verify.py`](../../scripts/language/ir_verify.py) the structural verifier (declared
instructions and fields, terminators, targets, regions, slots, callees, and one consistent operand
height per instruction, including at every handler), and
[`ir_machine.py`](../../scripts/language/ir_machine.py) the reference executor. The conformance
check lowers every execution vector, verifies it, executes it, and requires the same observable
sequence, terminal, and final state as the rule machine; each vector pins the digest of its IR. The
structural verifier does not yet check operand types, effect containment, or exception sets.

Only the verifier mints `FunctionCertified`; only a sealed module containing certified functions may
become `ModuleVerified`. Decoding serialized IR re-runs verification. Native code is a rebuildable
cache and never substitutes for verified IR.

## 14. Capabilities and selector grammar

Capability selectors use this closed grammar inside request braces:

```ebnf
selector = "root", "(", name, ")"
         | "arg", "(", name, ")"
         | string
         | "join", "(", selector, ",", selector, ")"
         | "narrow", "(", selector, ",", pattern, ")" ;
```

`join` requires a relative, non-traversing right operand. `narrow` intersects and cannot widen.
Patterns support literal segments, `*` within one segment, and terminal `**`. Parent traversal,
absolute prefixes in relative operands, malformed Unicode/platform prefixes, and symlink escape are
rejected.

Refined path types use `path<ROOT:"pattern">`, where `ROOT` is `workspace`, `project`,
`task.output`, or the binding of a host-issued `root<K>` such as `host-machine`. Types, effects,
grants, delegation, host enforcement, and audit MUST use the same normalized selector AST.

The execution rule is:

```text
inferred request <= declared request <= active grant <= host policy
```

The host rechecks the live policy and resource generation immediately before dispatch. External
effects are execute-once journal facts and are not rolled back with VM state.

### 14.1 Manifest and admission

A checked program has a **capability manifest**: every capability request in code reachable from
its entry callable, plus the names of the events it may emit and the operations it may await.
Reachable code is the entry body, every function it calls or names as a value, transitively, and
every lambda and fiber body inside that code. A request on a branch that will not be taken is in the
manifest; a function nothing refers to is not. A request whose arguments are all static is listed
with them; otherwise it stands for any arguments. An argument is **static** when it is a literal
written directly in argument position; a literal wrapped in any other form is not, and that
includes the unary negation a negative number reads as. The manifest contains at least the
capability part of the entry callable's effect row. It can contain more: a closure or fiber that
reachable code constructs and never runs still contributes its requests. It is derivable from
verified IR alone, so a host computes it there and does not rely on a frontend's account. Each
request instruction carries its static claim, the verifier checks the claim against the constants
before it, and a compiler that omits a claim only widens what must be granted.

A host that embeds the language decides before execution whether a program may start. Under
**preflight admission** every manifest request is tested against the grants. A request with static
arguments is covered by an unrestricted grant or by one that lists those arguments. A request with
non-static arguments is covered only by an unrestricted grant, unless a refined argument type or
selector bounds it statically. Every uncovered request is put to the host as a prompt, in manifest order;
a refusal does not stop the later prompts, so the user sees the whole list. An answer of allow
grants exactly that request for this execution. If any request stays uncovered the program is
**not admitted**: it does not start, emits no event, journals nothing, runs no cleanup, and has no
VM-local mutation to discard. Its transition sequence is the single entry `NotAdmitted`, and its
terminal has kind `not-admitted` and names the refused requests. Under **lazy admission** the program starts at once.

Admission never replaces the dispatch check above: an admitted program's requests are still tested
with their actual arguments against the live grant, which is what catches revocation.
[`admission.json`](semantics/admission.json) states these rules and
[`admission.py`](../../scripts/language/admission.py) is their reference; the execution vectors
pin every program's manifest and include refused, prompted, and admitted-then-revoked cases.

### 14.2 Sessions and turns

A host that keeps a session between submissions, as a REPL does, keeps a set of committed
declarations. Each submission is one turn. It is read, checked, and run against the declarations
committed so far together with its own, and its declarations are committed only if it reaches
`Complete`. A turn that fails, ends on a protected edge, is refused at admission, or is rejected by
the compiler commits nothing, including declarations that were themselves valid. Events it emitted
and requests it made before failing are not retracted; they are journal facts.

A function declared again in a later turn shadows the earlier one: later turns resolve the name to
the new revision, and functions committed earlier keep the revision they were checked against. No
committed declaration changes meaning, so nothing compiled against it is invalidated. A nominal type
has one identity in a session and declaring it again is `F-DIAG-DUPLICATE-DECLARATION`. A binding
introduced at the top of a submission scopes the rest of that submission only.

The readers see the session too. Co-Forth construction needs the stack signature of every word, so
it is given the signatures of the visible declarations; a word with no known signature is
`F-DIAG-UNBOUND-NAME` at construction, where the other frontends build a call that resolution
rejects with the same code. [`session.py`](../../scripts/language/session.py) is the reference and
[`session-vectors.json`](fixtures/session-vectors.json) holds the cases.

### 14.3 Effect binding

How a capability request is carried out is a property of how a program is built and hosted, not of
the program. There are two bindings, and the same checked program and IR run under either.

| | Mediated | Direct |
|---|---|---|
| A request is | a typed message to a host broker | a call to the linked provider |
| Grants | tested at admission and again at every request | tested once, against the manifest, when the program is built or admitted |
| Journal, replay acceptance, parking on a request | yes | no |
| Revocation takes effect | at the next request | not during a run |
| VM-local transaction | committed only by `Complete` | none |

Both bindings perform the same effects in the same order, reach the same terminal, and run the same
cleanup and drops; the direct binding spends nothing on mediation, as section 2 requires. Embedding
hosts and REPLs use the mediated binding. Native and JavaScript output use the direct binding.
Binding is independent of admission mode (hosted or unhosted, section 16) and of the execution
engine: a mediated program may be interpreted or compiled. The conformance check re-runs every
execution vector that does not depend on mediation under the direct binding and requires that
agreement.

### 14.4 Authority during compilation

Compile-time evaluation performs no run-time capability effect. The only host access compilation has
is through the compile-time operations that name it, `include-str` and `include-bytes`, and each is
a request against a compile-time grant that the host states separately from the run-time grants. A
request the compile-time grant does not cover is `F-DIAG-COMPILE-CAPABILITY-DENIED`, a compile error
at the requesting expression. Every file compilation read is recorded with its content digest as a
dependency of the declaration that read it, so a change to the file rechecks that declaration.

## 15. Concurrency memory model

Safe Finch is data-race free. Ordinary mutable places cannot be shared between concurrent
executions. `Transfer<T>` and `Share<T>` govern cross-domain ownership and sharing. Mutex, channel,
atomic, and other synchronization types establish specified happens-before edges.

Atomic operations require an ordering; the convenience default is sequential consistency. A weaker
ordering must be explicit and no weaker than the operation contract. Unsafe shared memory follows
the selected target model and is unavailable in hosted profiles.

Each execution owns its stack, frames, roots, budget, cancellation, and grant reference. A mutable
persistent-state operation lazily creates a revisioned transaction; an external effect lazily creates
an effect journal before exposure. Pure computation and ephemeral local mutation create neither.
Immutable modules/interfaces may be shared. Concurrent persistent-state commits use revisioned
transactions and report conflicts rather than racing on one global stack.

## 16. Portable ABI and FFI profile

Hosted and unhosted are execution-admission modes, not language profiles. Hosted admission rejects
any module containing an unsafe instruction or unsafe call edge, including unreachable code, and
permits foreign interaction only through safe typed host wrappers. Unhosted admission requires both
the build artifact and runtime policy to opt in. Unsafe code still receives no filesystem, process,
network, credential, or other host authority except through ordinary declared capability effects.
Every verified module records an unsafe summary used by admission and transitive linking.

Finch has two deliberately separate boundaries. The native-call ABI is target-bound and may use
registers, stack slots, indirect aggregates, call-scoped pointer-length borrowed views, and owned
results with matching release functions. The portable message ABI is target-independent serialized
data for process/RPC/effect boundaries; it has no native pointers or borrowed views. A conforming
implementation MUST NOT route a local native call through portable encoding, JSON, hex conversion,
or cryptographic hashing.

A result whose type is not an intrinsic scalar (a record, variant, fixed array, closure, or any
other aggregate) is returned in storage the caller provides. The caller reserves the slot in its own
frame and passes its address as a hidden argument; the callee constructs the result there directly.
Returning a constructor expression or a local therefore neither copies nor moves the value, and
this is guaranteed, not an optimization. A tail call passes the hidden address it received on to
its callee, so a proper tail call stays proper for aggregate results. The slot for a `join` or `step`
result belongs to the joining or stepping frame. Intrinsic scalars are returned in registers as the
target ABI directs. IR version 6 is a value-stack model and does not show the slot; the rule binds
every lowering of it to machine code.

A target ABI identity is the canonical tuple
`(architecture, operating-system, environment, object-format, C-ABI-revision, pointer-width,
endianness, scalar-layout-table, aggregate-layout-algorithm, calling-convention-set)`. Each textual
component is a versioned registry key, not a compiler marketing string. The canonical encoding and
its SHA-256 digest are recorded in every object, interface containing `repr(C)`, native cache entry,
and foreign-library declaration. Two `repr(C)` values are ABI-compatible only when their complete
identity digests and nominal declarations match. A target triple alone, host autodetection, or
matching size/alignment by accident is insufficient.

The language-defined portable message ABI has its own schema version.
Its wire scalar encodings are fixed-width little-endian two's-complement integers, IEEE binary32/64,
Unicode scalar values, and length-delimited bytes/UTF-8. Compound values cross only as a declared
`repr(stable N)` encoding or through generation-checked opaque handles. Every message is exactly one
tagged `call`, `result`, `effect`, or `resume` variant. Common fields are schema version, interface
digest, operation key, execution identity, generation, and sequence. Calls and effects carry typed
arguments; effects and resumes additionally carry request identity and effect sequence; only results
carry status and diagnostics.
The JSON schema represents every non-Boolean scalar as a tagged lowercase-hex byte sequence so JSON
number precision cannot alter wire identity. Operation keys are eight hex digits; generation and
sequence values are sixteen. UTF-8 payloads MUST decode strictly, and a `unicode-scalar-le` payload
MUST decode to one non-surrogate scalar. An inline scalar result has `inline` ownership and no
release operation; only an opaque owned result names its matching release operation; `none` carries
neither a value nor a release key. A raised result carries exception type identity and provenance. A
successful result has no diagnostic, while every non-success status carries a stable diagnostic code
and message.
Unknown required fields, versions, ownership modes, operation keys, or handle generations fail
closed before user code runs. There is no open extension map in 0.1; a new optional field requires a
new schema revision and its downgrade rule.

For each execution, the receiver persists `(generation, next-effect-sequence, accepted request
identities, terminal state)`. It accepts an effect or resume only when execution identity and
generation match, sequence is exactly next, request identity is new and bound to that sequence, and
the execution is nonterminal. It durably records acceptance before dispatch or resumption. An exact
duplicate returns the previously recorded acknowledgement/result without redispatch; stale,
skipped, conflicting, post-terminal, or wrong-generation messages fail closed. A resume is accepted
only for the one request identity the execution is currently parked on; a resume that names any
other identity is unsolicited and fails closed even when its generation and sequence are current.
Restart restores this state before any child resumes.
The executable acceptance table is [`replay-automaton.json`](semantics/replay-automaton.json), with
hostile duplicate, conflict, stale-generation, skipped-sequence, and post-terminal cases in
[`replay-transitions.json`](fixtures/replay-transitions.json).

Canonical JSON rejects duplicate keys, non-NFC keys, two keys colliding after NFC, lone surrogates,
non-scalar escapes, and noncanonical hex spelling. Objects sort by Unicode scalar key order after
validation; validation occurs before construction of a host dictionary so collision evidence cannot
be overwritten.

The machine-readable contracts are [`target-abi.schema.json`](schemas/target-abi.schema.json), its
versioned [`target-registry.json`](schemas/target-registry.json), and the target-independent
[`native-call-abi.schema.json`](schemas/native-call-abi.schema.json) and serialized
[`portable-message-abi.schema.json`](schemas/portable-message-abi.schema.json), both bound to the
sealed [`abi-operation-table.schema.json`](schemas/abi-operation-table.schema.json). Canonical interface and portable-message
instances with fixed digests live in [`schema-instances.json`](fixtures/schema-instances.json).
Registry keys never change meaning; a new tuple requires a new registry revision and fixtures.

No Finch exception, trap, cancellation, or cleanup unwind crosses a foreign frame. Export shims
catch them and return a declared status; a trap may put the affected instance into lockdown. A C
callback can be created only as an owned `ForeignCallback` containing a trampoline, stable context,
lifetime/revocation token, thread policy, and failure mapping. It cannot be obtained by casting an
arbitrary closure.

Foreign-thread calls attach through the runtime and enqueue a root invocation; they never reenter an
existing VM frame directly. Opaque pointers use typed generation-checked `ForeignResource<K>` values
with declared destruction. Sentinel and `errno` lifting is explicit wrapper metadata. Raw pointers,
C variadics, unchecked shared memory, and unverified symbols require `unsafe` and an unhosted profile.

Here “unhosted profile” means an unhosted admission mode combined with a profile that supplies the
required native/FFI operations; it is not a separately claimable version 0.1 language profile.

## 17. Diagnostics and conformance corpus

Diagnostics have stable code, phase, severity, primary and related source origins, expected/found
semantic values, module/environment revisions, trace, redacted state, cause, and remediation hints.
Reader, expansion, resolution, typing, ownership, verification, linking, authorization, execution,
commit, cancellation, and native failures use the same envelope.

The conformance corpus is normative alongside this document. Each feature includes:

1. canonical CoLisp and Co-Forth success fixtures where the feature is shared;
2. expected normalized semantic-construction events;
3. expected verified IR or canonical semantic digest;
4. observable result/effect/drop sequence;
5. the principal invalid program and stable diagnostic code; and
6. hostile control cases where ownership, suspension, cancellation, restart, or external effects are
   involved.

An implementation MUST pass every fixture in each profile it claims. Performance tests cannot
replace semantic assertions.

The version 0.1 corpus consists of these checked artifacts:

| Artifact | What an implementation must reproduce |
|---|---|
| [`grammar-corpus.json`](fixtures/grammar-corpus.json) | acceptance, and the reader code and byte offset of each rejection |
| [`execution-vectors.json`](fixtures/execution-vectors.json) | normalized AST, semantic digest, observable trace, terminal, and final state |
| [`static-rejections.json`](fixtures/static-rejections.json) | rejection before execution with the stated diagnostic code |
| [`replay-transitions.json`](fixtures/replay-transitions.json) | host-message acceptance rule, output, and next sequence |
| [`schema-instances.json`](fixtures/schema-instances.json) | canonical interface and portable-message instances and their digests |

`python3 scripts/language/check_language_spec.py` verifies every artifact against the others and is
the only supported way to regenerate derived fields (`--write`). A vector that exists in one
spelling only states why in `unpaired_reason`.

Tests are declarations, not module initialization or name-scraped functions. A test has a required
human-readable name and a stable identity derived from module plus nested suite/test names;
duplicates are compile errors. Discovery reads the separate test artifact without executing module
code. Co-located tests may access their module's private interface; external test modules are normal
black-box clients. Production sealing excludes tests and test-only evidence.

A test body is an ordinary checked callable receiving a scoped `TestContext`. Return is success;
an uncaught exception, trap, leaked owned task/fiber, unmet expectation, timeout, or cleanup failure
is a structured test failure. The runner owns the deadline, cancellation, output/effect capture,
deterministic seed, capability grants, fresh transaction, task tree, and cleanup scope. Tests are
isolated and order-independent by default; shared external state requires an explicit stable resource
key and synchronization policy.

`expect` is hygienic library syntax over typed matcher evidence. It evaluates its subject exactly
once and retains the expression origin for diagnostics. Fixtures are ordinary constructors returning
owned values and use lexical cleanup. Property tests record seeds and shrink traces. Snapshots are
versioned reviewable artifacts and never update merely because a test failed. Test doubles use
explicit static/dynamic concept evidence or injected callables; import replacement and global
monkey-patching are absent. Host-effect fakes operate at the typed effect/resume boundary and cannot
grant authority missing from the test policy.

## 18. Required prelude and extension rule

The exact required-prelude symbols and callable contracts live in the generated, versioned
[`spec-prelude.json`](spec-prelude.json). Its reviewed source is
[`prelude-definitions.json`](prelude-definitions.json), and
[`generate_spec_prelude.py`](../../scripts/language/generate_spec_prelude.py) must reproduce it
byte-for-byte. Illustrative names absent from that manifest are not
language features. `.` performs
static member/property/concept resolution; dynamic JSON lookup uses map/index or explicit
`MissingProperty` evidence.

Future syntax or semantic extensions require a new specification version, paired frontend mapping,
semantic-construction node, verifier rule, negative fixture, artifact-version behavior, and profile
gate. A backend optimization or library function does not require a language extension when it
preserves the existing observable contract.
