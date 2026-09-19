# D's Template System: Specialization, Constraints, IFTI, and the "Ad Hoc" Reputation

## What is the exact syntax for template specialization and partial specialization in D?

### Takeaway
D specialization is pattern-matching over a type parameter's shape (`template Foo(T : T[])`, `template Foo(T : T[U], U)`), which is a *separate mechanism* from template constraints (the `if (...)` clause); the compiler ranks specializations by "best fit" independent of constraint truth, then filters by constraints.

### Cited Findings
- Type specialization is written as a colon-constrained parameter: `template TFoo(T : T[]) { ... }` matches array types, and `template TFoo(T : char) { ... }` matches `char` specifically; the compiler "selects the one that is most specialized that fits the types." — [D Templates spec](https://dlang.org/spec/template.html)
- Partial specialization deduces multiple parameters from the shape of one argument: `template Foo(T : T[U], U) { ... }` then `Foo!(int[long])` deduces `T = int, U = long`. — [D Templates spec](https://dlang.org/spec/template.html)
- Template constraints are a distinct, separately-written clause: `void foo(T)(T value) if (is(T : int))`, or for aggregates `struct Bar(T) if (isIntegral!T) { ... }`. — [D Templates spec](https://dlang.org/spec/template.html); [Template Constraints article](https://dlang.org/articles/constraints.html)
- Value-parameter constraints look the same shape: `template Foo(int N) if (N & 1) { ... }` (odd `N` only). — [D Templates spec](https://dlang.org/spec/template.html); [Template Constraints article](https://dlang.org/articles/constraints.html)
- When several eponymous templates share a name, D disambiguates using a fixed specialization hierarchy — type parameters rank above value parameters, which rank above alias parameters, which rank above variadic/sequence parameters:
  ```d
  template Foo(T)         { }  // #1 type
  template Foo(int n)     { }  // #2 value
  template Foo(alias sym) { }  // #3 alias
  template Foo(Args...)   { }  // #4 sequence

  Foo!(3);     // instantiates #2 (value beats type)
  Foo!(std);   // instantiates #3 (alias beats sequence)
  Foo!(int,3); // instantiates #4 (only sequence matches multiple args)
  ```
  If two candidates are equally specialized, compilation fails as ambiguous. — [D Templates spec](https://dlang.org/spec/template.html)
- Constraints are explicitly **not** part of the specialization-ranking algorithm: "Constraints are not involved with determining which template is more specialized than another." Specialization/overload resolution happens first among all templates of the same name; constraints are then checked as a pass/fail filter on the resulting candidate. — [Template Constraints article](https://dlang.org/articles/constraints.html)
- Constraint bodies are ordinary D compile-time boolean expressions and may combine predicate template instantiations, `is()` expressions, and `__traits(compiles, ...)`:
  ```d
  T foo(T)(T t)
      if (isAddable!(T) && isMultipliable!(T))
  { return t + t * t; }

  template Foo(T, int N)
      if (isAddable!(T) && isPrime(N))
  ```
  and a hand-rolled predicate can be built directly on compilability:
  ```d
  const isAddable(T) = __traits(compiles, (T t) { return t + t; });
  ```
  — [Template Constraints article](https://dlang.org/articles/constraints.html)
- Overload sets can be constraint-partitioned instead of specialization-partitioned: `template Foo(int N) if (N & 1) { ... }` (A) and `template Foo(int N) if (!(N & 1)) { ... }` (B) act like a two-way compile-time dispatch over the same parameter shape, distinguished purely by their `if` clauses. — [Template Constraints article](https://dlang.org/articles/constraints.html)

### Inferences
- D therefore has **two independent, differently-scoped mechanisms** doing what a single "specialization" concept does in C++: (1) parameter-pattern specialization (`T : T[U]`), which is structural/syntactic matching on the shape of the argument, and (2) constraints (`if (...)`), which are boolean predicates evaluated after a best-fit candidate is chosen. A language designer copying this system needs to decide explicitly whether to keep these as one mechanism or two, since D keeps them orthogonal and this is a recurring source of surprise (see "ad hoc" question below — constraint order not affecting specialization rank is a common gotcha).
- The specialization-ranking hierarchy (type > value > alias > sequence) is a fixed, compiler-internal totalized order — it is closer to C++ partial ordering of template specializations than to a user-extensible mechanism; users cannot define custom "more specialized than" relationships the way C++20 concept subsumption allows.

### Gaps
- The spec fetch did not surface the exact wording D uses for is-expression specialization forms beyond `T : T[]`/`T : char`/`T : T[U], U` (e.g., specializing on delegate/function-pointer parameter shapes, or on a fixed number of tuple elements); a deeper read of the full `is()` expression grammar table on the same spec page would be needed for exhaustive coverage.

---

## What is IFTI and how does it let D infer template arguments from call arguments?

### Takeaway
IFTI is D's rule that a function template's type parameters are inferred from the runtime-argument types at the call site, so `!(...)` explicit instantiation becomes optional; this is analogous to C++ function template argument deduction but D leans on it far more heavily because of eponymous templates (below).

### Cited Findings
- "IFTI (Implicit Function Template Instantiation) refers to the ability to instantiate a template function without having to explicitly pass in the types to the template. Instead, the types are inferred automatically from the types of the runtime arguments." — [D Glossary](https://dlang.org/spec/glossary.html)
- Canonical example: `T square(T)(T t) { return t * t; }` then `square(3);` infers `T = int` with no `!(int)` needed. — [D Templates spec](https://dlang.org/spec/template.html)
- Three equivalent call forms illustrate the collapse from explicit to implicit: `min!(int).min(1, 2);` (fully explicit, non-eponymous form) → `min!(int)(1, 2);` (single-element/eponymous shortcut) → `min(1, 2);` (full IFTI, no explicit instantiation at all). — search synthesis citing [D Templates spec](https://dlang.org/spec/template.html)
- IFTI composes with eponymous templates for real Phobos functions: `string toUTF8(S)(S s)` can be called as `ws.toUTF8` (UFCS + IFTI) because "the compiler already knows that the argument to the function is a `wstring`. It doesn't need the programmer to tell it that." — [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/)

### Inferences
- IFTI in D is not a separate opt-in feature; it is simply what happens by default any time a function template parameter list's types are recoverable from argument types, which is why almost no D code ever writes `!(...)` for plain function templates — explicit instantiation is reserved for cases IFTI cannot resolve (return-type-only parameters, disambiguation, or non-function templates like `struct`/`enum`).

### Gaps
- No source found describing exactly how IFTI fails/what error is produced when inference is ambiguous (e.g., two parameters could each independently satisfy inference from different arguments) — worth a follow-up if the target language needs to specify inference-failure diagnostics precisely.

---

## How does `static if` work, and is there a real "static switch"?

### Takeaway
`static if` is compile-time-only conditional *inclusion* of code (not a runtime branch), and it composes only via chained `else static if`; D has **no native static-switch or compile-time pattern-matching construct** — this is a known, named forum complaint, and the community has repeatedly hand-rolled `match!(...)`-style template libraries instead of getting a language feature.

### Cited Findings
- "Static if is the compile time equivalent of the if statement... static if is not about execution flow; rather, it determines whether a piece of code should be included in the program or not." — [D-Summer-School: Static if](https://dlang-upb.github.io/D-Summer-School/meta-intro/static-if.html) (via search synthesis)
- A forum thread titled "static switch/pattern matching" opens with a direct complaint from a user (John): "Writing a long series of 'static if ... else' statements can be tedious and I'm prone to leaving out the crucial 'static' after 'else'." — [static switch/pattern matching thread](https://forum.dlang.org/thread/ugiypegvtdhhvzrmfuua@forum.dlang.org)
- No native static-switch exists; "D lacks a native static switch construct. Developers must rely on chained `static if`/`else static if` statements for compile-time type matching, which is error-prone and cumbersome." — [static switch/pattern matching thread](https://forum.dlang.org/thread/ugiypegvtdhhvzrmfuua@forum.dlang.org)
- Community workarounds are template-encoded, not language-level: Lodovico Giaretta proposed a `match!(...)` template taking alternating type-predicate/handler-lambda argument pairs, e.g. conceptually `match!(T, int, () {writeln("Matched int");}, is(T : SomeObject), () {writeln("Derives from SomeObject");})`, and Ketmar built a more elaborate `tyma` template handling inheritance checks via strings and a catch-all branch via `void`. — [static switch/pattern matching thread](https://forum.dlang.org/thread/ugiypegvtdhhvzrmfuua@forum.dlang.org)
- A real implementation difficulty surfaced in that thread: handler lambdas needed access to the *actual matched value* with its narrowed type, which Lodovico solved by passing the value as an `alias` template parameter so handlers could use the properly-cast value without triggering spurious compile-time type-conversion errors in branches that don't apply. — [static switch/pattern matching thread](https://forum.dlang.org/thread/ugiypegvtdhhvzrmfuua@forum.dlang.org)
- D separately has `static foreach`, explicitly designed by analogy: "static foreach was proposed as an addition to D, with static foreach being to foreach as static if is to if" (DIP1010). — search synthesis citing [DIP1010](https://github.com/dlang/DIPs/blob/master/DIPs/accepted/DIP1010.md)

### Inferences
- The absence of static-switch/pattern-matching-as-a-primitive, combined with the "leaving out the crucial 'static'" complaint, indicates D's compile-time-branching story is a thin, syntactically fragile layer over the runtime `if`/`else` grammar rather than a first-class DSL — a design a new language could improve on cheaply by making compile-time branching syntactically distinct (not reusing `if`/`else` keywords with an easy-to-forget modifier) or by providing real match/pattern syntax with type-narrowing bindings, which is exactly what the D community had to build by hand with alias-parameter tricks.
- The `alias` parameter workaround needed for typed match-arm bodies suggests that in a new design, compile-time pattern matching over types should have native support for binding a narrowed type/value inside each arm, since D users found this to be the hard, non-obvious part.

### Gaps
- Could not confirm whether `static switch` has since been added to the language (post-thread) or remains purely a rejected/unimplemented DIP; the thread found describes it as a discussion, not a shipped feature, but no direct search for "static switch DIP status 2024/2025" was run due to tool-call budget.

---

## What are D's template constraints, how are they composed, and is this concept-like?

### Takeaway
Constraints are a boolean `if (...)` predicate clause bolted onto a template/function signature; composition is done with ordinary `&&`/`||` over trait predicates (`isIntegral!T`, `isArray!T`, `hasMember!(T, "x")`) or raw `is()`/`__traits(compiles, ...)` expressions — this is structurally similar to a "concept" but, per named D veterans in the forums, it lacks the formal declaration, named-requirement decomposition, and good-error-message guarantees that C++20 concepts (and by extension Rust traits) provide.

### Cited Findings
- Basic syntax and composition, repeated for emphasis: `T foo(T)(T t) if (isAddable!(T) && isMultipliable!(T)) { return t + t * t; }` and multi-parameter: `template Foo(T, int N) if (isAddable!(T) && isPrime(N))`. — [Template Constraints article](https://dlang.org/articles/constraints.html)
- Constraints can be built from `is()` type-relation expressions (`is(T : float)`, `is(T == float)`) or from `__traits(compiles, ...)` probing whether an expression would type-check, e.g. a hand-written `isAddable`: `const isAddable(T) = __traits(compiles, (T t) { return t + t; });`. — [Template Constraints article](https://dlang.org/articles/constraints.html)
- Documented rationale for constraints existing at all: they give "better diagnostics when arguments don't match, rather than an obscure error message based on the irrelevant (to the user) internal details" of the template body. — [Template Constraints article](https://dlang.org/articles/constraints.html)
- Direct forum criticism that constraints are not concepts: Norbert Nemec: "I find it fairly difficult to come up with a clean solution for this that actually scales up for complex libraries," arguing D's constraint system lacks the formal structure of C++ concepts, which let you "state requirements readably in one place, allow implementations to declare compliance explicitly, and enable templates to specify needed concepts clearly." — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)
- Nemec's core technical complaint about boolean-AND composition: "Collecting individual requirements as an AND expression of booleans does not allow any helpful error message," and on the tradeoff of splitting constraints for better messages: "you win a meaningful error message but you lose the possibility for overloading." — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)
- Counterpoint from other veterans that Phobos conventions substitute for formal concepts: Ali Çehreli pointed to existing predicate templates like `std.range.hasLength` as a workable concept-like pattern already in the standard library; Andrei Alexandrescu: "Why not follow the pattern of isXxx in the standard library?" — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)
- Separately, on error-message quality specifically (a dedicated thread), Andrei Alexandrescu noted that when a constraint fails, the compiler telling you it doesn't match is "less, not more, clear to the user what steps to take to make the code work," i.e., the boolean-predicate design actively degrades diagnostics relative to just letting the body fail to compile with a body-level error. — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- Steven Schveighoffer, on a struct that almost satisfies `isInputRange` but fails silently (e.g., missing `@property` on a method): "to know why would be better" than just being told the type doesn't qualify — the constraint gives a pass/fail bit with no explanation of *which* structural requirement was unmet. — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- User "QAston" described real workflow pain: developers must either comment out library constraints or duplicate/recreate the predicate logic locally just to see a real compiler error, stating this makes them "hate template constraints because ... I prefer to be given real error[s]." — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- Proposed but apparently unshipped-at-time-of-thread fixes included: (1) detecting the first failing clause when a constraint is in conjunctive-normal-form and reporting only that clause; (2) a `pragma(err)`-style explicit in-constraint error annotation (rejected by author as "tedious" since it requires modifying every constraint); (3) buffering all suppressed sub-errors during constraint evaluation and exposing them under a verbose flag — Walter Bright's concern with the latter was that it risks becoming "a rather large and unstructured pile of messages." — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)

### Inferences
- D's constraints are "concept-like" in that they let you name a compile-time-checkable requirement and attach it to a template (exactly the ergonomic goal of C++ concepts and Rust traits), but they are missing three things C++20 concepts/Rust traits have: (1) a **named, declared** requirement surface (a concept/trait is itself a first-class, introspectable declaration; a D constraint is just an inline boolean expression with no separate identity beyond the predicate templates it calls), (2) **structured subsumption/refinement** for overload resolution beyond the fixed type>value>alias>sequence hierarchy (Nemec's "you lose the possibility for overloading" complaint), and (3) **guaranteed sub-requirement diagnostics** — because it's a boolean AND, the compiler by design cannot know which conjunct failed without extra machinery, whereas a Rust trait bound failure names the exact missing trait/method.
- The repeated, decade-spanning nature of the error-message complaint (the "ad hoc" criticism thread and the dedicated error-message thread make essentially the same point years apart, with no shipped resolution described in either) is itself evidence for the "ad hoc" reputation: the community has repeatedly diagnosed the root cause (boolean predicates erase structure) and proposed multiple competing fixes (CNF-first-clause, `pragma(err)`, buffered verbose output) without the language absorbing a single clear winner — the opposite of a designed-in-advance concept system.

### Gaps
- Did not find confirmation of whether any of the three proposed fixes for constraint diagnostics actually shipped in a later DMD version; the PR referenced by search ("Improve template constraint error messages by dayllenger · Pull Request #9715 · dlang/dmd") was not fetched and its merge status/date is unconfirmed.
- Did not find a primary C++20-concepts-vs-D-constraints technical comparison document (e.g., a blog post enumerating subsumption rules side by side); the forum thread is opinion/discussion, not a systematic feature comparison.

---

## What is an eponymous template and why does it matter for ergonomics?

### Takeaway
An eponymous template is a template whose single member shares the template's own name; the compiler then lets you refer to the instantiation itself (`Foo!(T)`) as if it *were* that member, eliminating an otherwise-mandatory extra `.member` access and enabling D's terse, "just call it like a function" style for generic functions, structs, and constants.

### Cited Findings
- Definition, from the language spec: "the most common form of template declaration is the single-member eponymous template" — a template declaring a symbol with the same identifier as the enclosing template is assumed to be the thing referred to on instantiation. — [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/); [D Templates spec](https://dlang.org/spec/template.html)
- Longhand vs. shorthand for a function template:
  ```d
  // longhand
  template max(T) {
      T max(T a, T b) { ... }
  }
  // eponymous shorthand — identical meaning
  T max(T)(T a, T b) { ... }
  ```
  — [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/)
- Eponymous member access example from the spec:
  ```d
  template foo(T)
  {
      T foo;  // eponymous member
  }
  foo!(int) = 6;  // implicitly accesses foo!(int).foo
  ```
  — [D Templates spec](https://dlang.org/spec/template.html)
- The shorthand syntax "does not depend on the kind of template parameters you use and is the same for classes, structs, and functions," e.g. `struct MyStruct(T, U) { T t; U u; }`. — [D Templates spec](https://dlang.org/spec/template.html); [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/)
- Eponymous templates additionally license a call-syntax shortcut: when a single template argument is a single lexical token, the instantiation parentheses can be dropped, e.g. `to!int("42")` instead of `to!(int)("42")`. — [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/)
- Eponymous templates are what make IFTI feel invisible in practice: for `string toUTF8(S)(S s)`, a caller writes `ws.toUTF8` and gets full inference with zero template-instantiation syntax visible at the call site. — [The ABC's of Templates in D](https://blog.dlang.org/archive/2020/07/31/the-abcs-of-templates-in-d/)

### Inferences
- Eponymous templates are best understood as sugar that collapses "template declaration containing one thing" into "that thing, generically parameterized," which is why virtually all D generic *functions* in Phobos are written in the eponymous shorthand form rather than the explicit `template X(...) { ... }` form — the explicit form is reserved for templates with multiple members (e.g., a template bundling a type alias plus helper functions) or non-eponymous metaprogramming templates.
- This sugar is also a source of subtlety noted in passing by a forum issue title found in search ("Eponymous template FQN's re-state the template name," issue #19959) — fully-qualified-name resolution and multi-declaration eponymous templates (a template with a same-named function *and* other overloads) have edge cases not covered in the sources fetched here.

### Gaps
- Did not fetch the issue #19959 thread itself, so the exact FQN restatement bug/edge case is unconfirmed in detail — only the title was seen in search results.

---

## What specifically makes D's template system feel "ad hoc" to experienced users?

### Takeaway
The "ad hoc" reputation, per named veteran D users across two separate multi-year forum threads, traces to a specific, repeatable pattern: constraints are unstructured boolean expressions with no separate declared identity, so (a) failures don't say *which* sub-requirement failed, (b) splitting constraints for better errors costs you overloading, (c) there is no compiler-enforced single source of truth for "what does this type need to support" the way a C++20 concept or Rust trait declaration is, and (d) proposed fixes have stayed as competing forum proposals rather than a converged language feature.

### Cited Findings
- Root complaint (structural, not cosmetic): "Collecting individual requirements as an AND expression of booleans does not allow any helpful error message" — Norbert Nemec. — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)
- Tradeoff complaint baked into the design: "you win a meaningful error message but you lose the possibility for overloading" — Norbert Nemec, describing that if you split one constrained overload into several narrower ones to localize errors, you collide with D's overload-resolution rules. — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)
- Alexandrescu himself (a co-designer of the constraint feature) conceded the diagnostics can be actively worse than no constraint at all: constraint failure is "less, not more, clear to the user what steps to take to make the code work." — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- Practical developer workaround reported: QAston says people resort to commenting out library constraints or re-implementing the predicate locally just to get the compiler to show a real error, and states plainly they "hate template constraints" for this reason. — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- Multiple non-converged proposed fixes stayed unresolved as of the fetched thread: CNF-first-failing-clause detection, a `pragma(err)` explicit-message annotation (rejected as tedious to add to every constraint), and full sub-error buffering exposed under a verbose flag, which Walter Bright worried would become "a rather large and unstructured pile of messages." — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html)
- Separately, the *specialization-vs-constraint* split itself is a documented, non-obvious rule that can surprise users: "Constraints are not involved with determining which template is more specialized than another" — meaning two mental models (pattern-shape specialization and boolean-predicate constraints) run as separate compiler passes with separate rules the user must hold simultaneously. — [Template Constraints article](https://dlang.org/articles/constraints.html)
- The static-if/static-switch gap (see above) compounds the "ad hoc" feel at the control-flow level too: no native compile-time pattern matching exists, so type-dispatch logic is either a fragile `static if`/`else static if` chain (with the "leaving out the crucial 'static'" foot-gun) or a bespoke, alias-parameter-based `match!(...)` template that each project reinvents. — [static switch/pattern matching thread](https://forum.dlang.org/thread/ugiypegvtdhhvzrmfuua@forum.dlang.org)
- Countervailing view: Ali Çehreli and Andrei Alexandrescu argue the "ad hoc-ness" is mitigated in practice by strong social convention — the Phobos `isXxx`/`hasXxx` naming pattern (`hasLength`, etc.) gives constraints a de facto (not compiler-enforced) concept vocabulary that the community reuses instead of each library inventing its own. — [Concepts vs template constraints thread](https://forum.dlang.org/thread/jabakh$te4$1@digitalmars.com)

### Inferences
- The unifying theme across both threads is: **D chose to make constraints "just expressions" for maximum flexibility (any D boolean compile-time expression is legal), and that same flexibility is the thing that prevents the compiler from structurally decomposing a failure** — this is a direct tradeoff a new language's designer should treat as a first-class decision point (structured trait/concept declarations trade some expressiveness for guaranteed per-requirement diagnostics and subsumption; free-form boolean constraints trade guaranteed diagnostics for unlimited expressiveness including arbitrary compile-time function calls like `isPrime(N)`).
- "Ad hoc" as used by experienced D users does not mean "unprincipled" so much as "under-formalized": the mechanisms (specialization, constraints, `static if`, eponymy, IFTI) are individually well-specified and powerful, but there is no unifying declared-contract abstraction (no `concept`/`trait` keyword) tying them together, so consistency across a codebase depends on naming convention (`isXxx`) rather than the compiler.

### Gaps
- No direct quote was found using the literal word "ad hoc" from a named D community member; the characterization is this researcher's synthesis of the cited structural/error-message/control-flow complaints, which is why it is stated as an inference rather than a cited claim. Flag this to the report writer: the word "ad hoc" appears to be the task's own framing, not a verified verbatim community term — the underlying substance (unstructured boolean constraints, poor sub-requirement diagnostics, no static pattern matching, two independent specialization/constraint passes) is well-sourced above.

---

## Is D's template matching nominal, or does it also support structural matching?

### Takeaway
D supports both: parameter-shape specialization and `is()`/base-class checks are nominal/structural-on-syntax, but the dominant idiom in Phobos (`isInputRange!T` and friends) is genuine **structural/duck-style typing** — a type qualifies purely by having the right members with the right signatures, with no inheritance, interface, or explicit declaration of conformance required.

### Cited Findings
- `isInputRange` is defined purely in terms of required operations: an input range is "something from which one can sequentially read data using the primitives front, popFront, and empty," and the compile-time check is literally: "R r; if (r.empty) {} r.popFront(); auto h = r.front;" — i.e., "does this code compile for R?" — [std.range.primitives docs](https://dlang.org/phobos/std_range_primitives.html)
- A type satisfies `isInputRange` with zero nominal declaration of intent — no base class, no interface, no attribute:
  ```d
  struct B
  {
      void popFront();
      @property bool empty();
      @property int front();
  }
  static assert(isInputRange!B);
  ```
  — [std.range.primitives docs](https://dlang.org/phobos/std_range_primitives.html)
- This structural checking has a sharp edge noted in forum discussion: because there is no declared conformance, a small mistake silently fails the check rather than erroring at the mistake site — a method missing its `@property` annotation, or a member misspelled, simply makes the type not match `isInputRange` with no pointer to *why*, which is the same diagnostic weakness described in the constraints section above. — [Simple and effective approaches to constraint error messages](https://digitalmars.com/d/archives/digitalmars/D/Simple_and_effective_approaches_to_constraint_error_messages_283930.html) (Schveighoffer's example); general framing also noted in search synthesis referencing D range articles
- By contrast, specialization forms like `template TFoo(T : char)` and inheritance-based `is(T : Base)` checks are closer to nominal matching — they test against an explicit named type or explicit base-class relationship rather than an implicit member-shape contract. — [D Templates spec](https://dlang.org/spec/template.html)

### Inferences
- D's structural checking is implemented via `__traits(compiles, ...)`-style "does this expression type-check" probing (as seen in the hand-rolled `isAddable` example under Constraints), which is the same general technique C++ duck-typed templates and TypeScript's structural type system both use in spirit — but D's version is opt-in and expression-based rather than being the type system's default assignability rule (TypeScript structurally checks *all* object types by default; D structurally checks only what a specific constraint's `__traits(compiles,...)` or is-expression happens to probe).
- The combination of (structural checking) + (no compiler-mandated single declaration point for "what does this concept require") is the mechanical root of both the ergonomic power (any subset of behavior can be probed, cheaply, per call site) and the "ad hoc" complaint (two types can each satisfy `isInputRange` for different, not-obviously-related reasons, and a failing type gets no explanation of which member/signature it's missing).

### Gaps
- Did not find a primary-source article explicitly using the phrase "duck typing" to describe D ranges (the search-tool synthesis referenced the concept via a Hacker News thread and Wikipedia rather than a D-authored primary source); treat "duck typing" as this researcher's/community's descriptive label rather than D's own official terminology — the official terminology in the D docs is simply "input range primitives" / structural requirement, not "duck typing."
