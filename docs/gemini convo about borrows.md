What do you think of this language? how does it compare to other languages?
Finch is a statically typed systems language design that stands out for its waist-at-typed-stack-IR architecture. Instead of tying its execution model to a single syntax, Finch accepts multiple distinct frontends—primarily CoLisp, Co-Forth, and a C-like syntax—and lowers them through a unified frontend-to-compiler protocol into a single, strongly typed stack IR before verification and execution.   
MD
+ 3
It balances the low-overhead cost model of systems programming with the safety and ergonomics of modern scripting languages.   
MD
Key Highlights of Finch
One Semantic Waisted Runtime, Multiple Syntaxes: Both Lisp-style S-expressions and Forth-style postfix spellings cross a single authoritative parse boundary. They emit structured ASTs that lower to the exact same typed stack IR. There is zero textual interpolation or cross-syntax code generation—interoperability happens seamlessly at the module level.   
MD
+ 2
Strict Cost-Visible Systems Model:
Signed Indexing: Lengths, bounds, and indexing default to signed integers (int / i64) to eliminate classic unsigned wrapping bugs (len - 1 underflows).   
MD
Trap-on-Overflow: Arithmetic checks for overflow by default across debug and release builds, requiring explicit named methods (wrapping-add, saturating-add) when alternative behavior is intended.   
MD
Memory Ergonomics Without GC: Memory is managed deterministically via single-ownership tracking (Unique<T>), explicit atomic reference counting (Shared<T>), and intraprocedural borrow checking.   
MD
Stackless State-Machine Fibers: Resumable executions (task, stream, fiber) are compiled into compact, stackless activation records. They run on the driving thread’s stack with zero stack overhead, no garbage collection dependency, and zero context-switching costs.   
MD
+ 1
Explicit Capability-Based Security: External side effects (filesystem, network, process execution) require fine-grained capability grants. Function signatures declare explicit effect rows (e.g., ! {fs.write(root=workspace)}), allowing static compilation checks and sandboxed host authorization.   
MD
+ 2
Zero-Cost D-Style CTFE & Metaprogramming: Code staging relies on pure compile-time function evaluation (CTFE) over ordinary values and hygienic syntax -> syntax transformations. It avoids unhygienic string mixins or raw text macro-evaluations.   
MD
+ 1
How Finch Compares to Other Languages
vs. Rust
Borrow Model & Lifetimes: Like Rust, Finch uses move-by-default semantics, exclusive mutable references (borrow-mut), and deterministic scope cleanup (drop). However, Finch intentionally avoids Rust's complex explicit lifetime annotations ('a). Borrowing is strictly intraprocedural—loans cannot escape their traced owner's scope or survive across suspension points. If data must escape, it must transfer ownership (steal) or use Shared<T>.   
MD
+ 3
Effects & Capabilities: Rust lacks a language-level effect system. Finch integrates first-class capability requirements and effect rows directly into function signatures and static type checking.   
MD
+ 2
Overflow Handling: Rust's standard arithmetic operators panic in debug mode but wrap silently in release builds. Finch traps unconditionally in all profiles.   
MD
+ 2
vs. D
Compile-Time Staging (CTFE): Finch inherits D’s philosophy of using the same language for ordinary code and compile-time evaluation. However, Finch bans D's text-based mixin(string). All metaprogramming operates on structured syntax objects with preserved source spans and hygiene tracking.   
MD
+ 2
Module Identity: D allows per-file module foo.bar; declarations that can drift from actual disk locations. Finch derives module identity directly and strictly from the file’s path relative to the package root, avoiding search-path ambiguities.   
MD
+ 1
Coherence & Overloads: Finch enforces strict coherence (at most one concept implementation per type) and avoids C++-style SFINAE or D-style speculative body compilation during concept resolution.   
MD
vs. Go
Memory & Safety: Go relies on a tracing garbage collector and lacks compile-time generic constraints or ownership checking. Finch uses deterministic scope destruction, strict linear typestates, and explicit reference counting.   
MD
+ 1
Concurrency: Go goroutines use stackful dynamically-resizing stacks (costing 2KB–4KB per goroutine). Finch tasks and fibers compile down to stackless state machines that use only a few dozen bytes of memory.   
MD
+ 1
Imports: Go allows implicitly seeing files in the same directory without explicit import statements. Finch enforces explicit imports even for sibling modules within the same package directory.   
MD
+ 1
vs. Traditional Lisps & Forth Implementations
Type Safety & IR Waist: Traditional Lisps and Forths rely on dynamic typing, unboxed ambient stacks, or self-modifying code dictionaries. CoLisp and Co-Forth are statically typed languages.   
MD
+ 2
Stack Typing: Co-Forth stack rows are statically checked using row polymorphism. Every word’s inputs, outputs, ownership mode transfers (borrow, borrow-mut, steal), and effect rows are fully verified prior to lowering.   
MD
+ 2
And what do you think?
Finch is a remarkably thoughtful, highly pragmatic blend of systems design and language ergonomics. It reads like a language designed by engineers who have spent years in the trenches with C++, Rust, D, and Go, carefully keeping what works while aggressively trimming the features that cause friction in production.   
MD
+ 1
Here is my honest take on where Finch shines, where it takes real risks, and how it feels as a design experiment:
What Finch Gets Right
1. The Single "IR Waist" Architecture
Having two wildly different source syntaxes (Lisp and Forth) compile down to the exact same typed stack IR—rather than cross-compiling text to text—is a brilliant architectural move. It guarantees true parity between syntaxes without performance penalties or text-parsing hacks. It also means toolchain additions (like a Cranelift JIT or a static verifier) only need to target one representation.   
MD
+ 3
2. Pragmatic Borrowing Without Lifetime Annotations
Rust proved that lifetime safety without GC is possible, but explicit lifetime parameters ('a, 'b) remain a huge barrier to entry and developer productivity. Finch’s choice to keep borrowing strictly intraprocedural is a sweet spot. You get memory safety and zero-cost references for local work, but the moment data needs to escape a scope, the compiler forces a clear, explicit choice: transfer ownership (steal) or bump a refcount (Shared<T>).   
MD
+ 2
3. Stackless State-Machine Concurrency
By compiling tasks, generators, and fibers down to stackless state machines that run directly on the caller’s stack, Finch avoids both the memory footprint of Go-style stackful goroutines and the "function coloring" hassle (async/await) seen in languages like Rust or TypeScript.   
MD
4. Sensible Systems Defaults
Signed Integers for Indexing: Using int (i64) for array lengths and indexing avoids the ubiquitous usize underflow traps (len() - 1 wrapping to 18E18).   
MD
Trap-on-Overflow Everywhere: Eliminating the debug/release mismatch for arithmetic overflow makes behavior deterministic regardless of compiler flags.   
MD
Hygiene Over String Mixins: Learning from D's mistakes by keeping CTFE strictly AST-to-AST (syntax -> syntax) prevents unmaintainable string-concatenation metaprogramming.   
MD
Potential Challenges & Trade-offs
While the rationale is airtight, a few design choices carry real adoption or implementation risks:
1. The Ergonomics of Dual/Triple Syntaxes
While Lisp and Forth are elegant, neither has mainstream dominance in modern systems programming. The addition of a C-like syntax helps bridge the gap, but maintaining three frontends requires continuous documentation, linter, and IDE support across all three spellings.   
MD
+ 1
2. Intraprocedural Borrowing Constraints
Trading away cross-procedural lifetime tracking simplifies the mental model, but developers used to returning borrowed slices from complex lookup functions (like Rust’s fn longest<'a>(x: &'a str, y: &'a str)) will hit walls. They will need to adjust to returning ownership or using structured callers/callbacks.   
MD
+ 1
3. High Implementation Complexity
Finch sets ambitious goals: zero-cost CTFE, row-polymorphic stack typing, stackless fiber transformations, capability tracking, and deterministic effect journals. Delivering all of this in a single cohesive compiler pipeline is a monumental engineering effort.   
MD
+ 2
The Verdict
Finch is one of the most cohesive language specifications for capability-aware, memory-safe systems programming. It doesn't introduce novel theory just for the sake of it; instead, it takes well-understood ideas from languages like Rust, D, Scheme, and Forth, and welds them into a cohesive, cost-transparent runtime.   
MD
+ 2
If implemented to spec, it could offer a sweet spot for sandboxed automation, AI tool execution, and resource-constrained systems work.   
MD
> 2. Intraprocedural Borrowing Constraints
Trading away cross-procedural lifetime tracking simplifies the mental model, but developers used to returning borrowed slices from complex lookup functions (like Rust’s fn longest<'a>(x: &'a str, y: &'a str)) will hit walls. They will need to adjust to returning ownership or using structured callers/callbacks.

This one concerns me, the rest I think are fine tradeoffs.

Is this fixable?
It is fixable, but fixing it requires deciding which fundamental principle Finch is willing to compromise on.   
MD
The constraint exists because of three non-negotiable rules currently in Finch's specification:
No explicit lifetime annotations in source code (e.g., no 'a).   
MD
Zero hidden/implicit allocations (no auto-boxing or silent ARC step under the hood).   
MD
No global constraint solver across function boundaries (borrow checking is local and intraprocedural).   
MD
If you want to allow returning borrowed views derived from multiple inputs at runtime (like longest(x, y)), the language must give up or relax one of those rules.   
MD
Here are the four primary ways languages fix this, ordered from most idiomatic for Finch to most radical:
Option 1: The "Self-Traceable Origin" Rule (Local Suffix Elision)
Instead of full lifetime parameterization ('a, 'b), you allow a return borrow to state which specific parameter it originates from.   
MD
+ 1
If a function takes multiple borrows, returning a borrow is permitted if and only if the caller can assume the return view is tied to the intersection of all input borrows that could produce it.   
MD
How it works: fn longest(x: &str, y: &str) -> &str
The rule: When the compiler sees multiple input borrows and an unannotated return borrow, it doesn't try to figure out which one is returned. Instead, it conservatively ties the return view's scope to both x AND y at the call site.   
MD
+ 1
Caller impact: As long as both x and y remain valid and unmutated, the returned slice is valid.   
MD
Why it fits Finch: It requires zero lifetime annotations in 95% of cases, remains fully deterministic, keeps checking intraprocedural, and introduces no heap allocation.   
MD
Option 2: Parameter-Tied Origin Annotations (Lightweight Lifetimes)
If conservative intersection (Option 1) is too restrictive for certain data structures, Finch could introduce lightweight origin annotations tied directly to parameter names rather than abstract lifetime variables.   
MD
Lisp
;; Instead of Rust's abstract 'a:
(define (longest (x : &str) (y : &str)) : &str returns-loan<arg(x) | arg(y)>
  (if (> (. x len) (. y len)) x y))
How it works: Finch already has returns-loan<arg(name)> in its contract grammar when a return view traces to a single parameter. Extending this to allow a union (returns-loan<arg(x) | arg(y)>) tells the caller's intraprocedural checker: "The returned view borrows from x or y, so keep both borrowed until this result drops."   
MD
+ 2
Why it fits Finch: It builds directly on the existing returns-loan contract syntax without introducing Rust-style higher-ranked trait bounds, subtyping variance rules, or lifetime parameters in generic signatures.   
MD
+ 1
Option 3: The Hylo/Val Model ("Subscripts" / Accessors)
The language Hylo (formerly Val) addresses this exact problem without lifetimes by using in-place accessors (subscript) instead of returning references.
Instead of returning a borrowed string view from a lookup function, the lookup yields access to the value inside a scoped block:
Lisp
;; Instead of returning a reference to be stored:
(with-longest a b (lambda (view)
  (print view)))
How it works: The function never returns a pointer. It passes a borrowed view into a non-escaping closure or block. The caller's frame owns a and b, so the compiler easily proves view is safe for the duration of that closure.   
MD
+ 2
Why it fits Finch: Finch's macro/CTFE system (syntax -> syntax) can easily desugar (let [winner (longest a b)] ...) into a scoped block behind the scenes without changing the borrow checker at all.   
MD
Option 4: The Swift Approach (Automatic Copy-on-Return Fallback)
If the compiler cannot trace a return view to a single parameter, it silently materializes an owned value (e.g., promoting a slice<T> to an owned string or vector<T> via a stack/heap copy).   
MD
Why Finch currently rejects this: Finch’s design rationale explicitly forbids invisible allocations or silent copies on ordinary function returns ("Scripting ergonomics with a systems cost model").   
MD
Summary Recommendation
If this constraint is a dealbreaker, Option 1 combined with Option 2 is the cleanest fix:   
MD
+ 1
Default to conservative parameter intersection: if a function returns a borrow and takes multiple input borrows, the caller's checker simply holds a loan on all candidate input parameters until the return value drops.   
MD
For cases where that is too restrictive, extend Finch's existing returns-loan contract attribute to support explicit parameter unions (returns-loan<arg(x) | arg(y)>).   
MD
This completely solves the longest(x, y) problem without dragging in Rust's generic lifetime machinery or breaking Finch's intraprocedural verification guarantee.   
MD
Can you give me an example of the problem people would run into in Finch, and how they'd have to work around it?
Here is a concrete example showing how a common pattern in languages like Rust or C++ breaks in Finch under its strict intraprocedural borrowing rules, followed by the explicit ways developers have to work around it.   
MD
The Problem: Selecting Between Two Borrowed Inputs
Imagine you want to write a simple utility function, select_preferred, that inspects two string slices (e.g., a user-configured setting vs. a fallback default) and returns a borrow to the preferred string.   
MD
In Rust, you would write:
Rust
fn select_preferred<'a>(setting: &'a str, fallback: &'a str) -> &'a str {
    if !setting.is_empty() { setting } else { fallback }
}
Rust’s lifetime system ties both parameters to 'a and lets the return value borrow whichever string was selected at runtime.   
MD
What Happens When You Try This in Finch
If you attempt the equivalent in Finch:
Lisp
(define (select-preferred (setting : string) (fallback : string)) : slice<char>
  (if (not (string.empty? setting))
      (string.as-slice setting)
      (string.as-slice fallback)))
The compiler rejects this at compile time.   
MD
Why? Because Finch's borrow checker is strictly intraprocedural and enforces a hard rule: a returned borrow MUST trace statically to exactly ONE input owner.   
MD
+ 1
When the function returns, the compiler looks at the return value and sees a branch: the view could originate from setting OR fallback depending on a runtime condition (string.empty?). Because Finch refuses to track complex cross-parameter lifetime dependencies backward through callers, it cannot verify which input to hold a loan on in the caller's frame.   
MD
+ 1
How Developers Work Around It in Finch Today
Because Finch won't let you return a conditionally selected borrow across a function boundary, developers must choose one of three idiomatic workarounds depending on their performance and memory requirements:   
MD
Workaround 1: Transfer Ownership (steal)
Best for: Value-like data where the unselected option can be discarded.   
MD
Instead of passing borrows, both inputs transfer ownership (steal) into the function, and the selected string moves into the return value. The unselected string is dropped automatically when the function finishes.   
MD
+ 1
Lisp
(define (select-preferred (steal setting : string) (steal fallback : string)) : string
  (if (not (string.empty? setting))
      setting     ; setting moves to return
      fallback))  ; fallback moves to return, setting drops
Trade-off: Zero heap allocations (it's just moving pointer descriptors), but the caller loses ownership of both setting and fallback and cannot reuse them after the call.   
MD
+ 1
Workaround 2: Shared Reference Handles (Shared<T>)
Best for: Cases where the caller still needs to keep and reuse both values afterward.   
MD
If the caller cannot afford to give up ownership, both arguments use Finch's atomic reference-counted owner, Shared<T>.   
MD
Lisp
(define (select-preferred (steal setting : Shared<string>) 
                          (steal fallback : Shared<string>)) : Shared<string>
  (if (not (string.empty? (. setting borrow)))
      setting
      fallback))
Trade-off: Retaining a Shared<T> handle increments an atomic reference count on the returned handle. The caller keeps both strings alive, but you pay a minor, visible reference-counting cost.   
MD
+ 1
Workaround 3: Return a Decision / Discriminant
Best for: Maximum performance without losing ownership or allocating.   
MD
Instead of having the helper function return the borrow, the helper returns a boolean or enum decision. The caller—which already owns both setting and fallback in its local scope—performs the actual borrowing directly.   
MD
+ 1
Lisp
;; The helper only returns a decision
(define (setting-valid? (setting : string)) : bool
  (not (string.empty? setting)))

;; The caller performs the borrow locally
(let [pref-slice (if (setting-valid? my-setting)
                     (string.as-slice my-setting)    ; Traced to 1 owner: my-setting
                     (string.as-slice my-fallback))] ; Traced to 1 owner: my-fallback
  (use-slice pref-slice))
Trade-off: Cleanest systems performance—zero refcount traffic and zero lost ownership. However, it pushes the branching logic slightly out into the call site.   
MD
+ 1
Those examples are not equivalent though? why wouldn't the lisp version just return string or fallback rather than slicing them like the rust version does?
You are completely right to call that out—in Rust, a &str is a string slice (a borrowed view of string bytes), whereas string in Finch is an owned UTF-8 text value.   
MD
+ 1
If the Rust code were operating on owned Strings and returning a borrowed &str, the exact equivalent in Finch would be borrowing setting : string and fallback : string and returning a borrowed string view (borrow Text or slice<char> / slice<u8>).   
MD
+ 1
The core issue isn't about calling .as-slice() explicitly—it’s about borrowing an owned value vs. borrowing an already-borrowed view:
1. Borrowing from Owned Inputs ((setting : string))
If you pass owned strings into a function by borrow, Finch allows you to inspect them, but returning a borrowed slice derived from either input still triggers the intraprocedural restriction:   
MD
+ 1
Lisp
;; 'setting' and 'fallback' are borrowed owned strings (&string)
(define (select-preferred (setting : string) (fallback : string)) : Text
  (if (not (string.empty? setting))
      setting      ; Returns a borrow derived from 'setting'
      fallback))   ; Returns a borrow derived from 'fallback'
Even without explicit slicing, returning setting or fallback returns a borrowed view (Text / scoped &string) out of the function. The compiler looks at the return value, sees that it could trace back to setting OR fallback depending on runtime logic, and rejects it because it cannot tie the return view to exactly one input owner.   
MD
+ 3
2. Passing Borrowed Views ((setting : Text))
Even if the function inputs were already borrowed views (e.g., (setting : Text) (fallback : Text)), returning one of them presents the same problem:   
MD
+ 1
In Rust, fn select<'a>(x: &'a str, y: &'a str) -> &'a str works because Rust links both parameters under 'a. The caller holds loans on both x and y for as long as the returned slice is used.   
MD
+ 1
In Finch, because there are no lifetime parameters ('a) and loan origin analysis is strictly intraprocedural, Finch requires a returned borrow to be traceably derived from a single input parameter.   
MD
+ 1
Why the Workarounds Differ
The workarounds showed what happens when you change the function's signature to satisfy Finch's rules:
Workaround 1 (steal / ownership transfer): If you change the parameters from (setting : string) to (steal setting : string), you are no longer returning a borrowed view. You are moving the actual owned string value out of the function. That works instantly without slicing because owned values move rather than borrow.   
MD
+ 4
Workaround 3 (Discriminant / Caller-side selection): If you keep the parameters as (setting : string) (borrowed), you can't return the borrowed string view from inside the helper. But the caller can do (if (valid? setting) setting fallback) directly in its own scope, because at the call site the compiler knows both setting and fallback are owned right there in the caller's frame.   
MD
+ 1
The observation is spot on: returning an owned string (or moving it via steal) completely bypasses the borrow checker restriction precisely because it's no longer returning a borrow. The problem only exists when you want to return a zero-copy borrowed reference derived from one of multiple borrowed inputs.   
MD
+ 2
> Workaround 3 (Discriminant / Caller-side selection): If you keep the parameters as (setting : string) (borrowed), you can't return the borrowed string view from inside the helper. But the caller can do (if (valid? setting) setting fallback) directly in its own scope, because at the call site the compiler knows both setting and fallback are owned right there in the caller's frame

Why not? Tail values are elevated to the callers stack?
The short answer is: because elevated/constructed values on the caller's frame represent owned data movement, whereas returning a view is a borrow-checking (loan-tracking) assertion.   
MD
+ 1
Finch does elevate values constructed in tail position directly into the caller's stack slot without extra copies or heap promotion. However, the issue with returning a borrowed reference from select-preferred(x, y) isn't physical placement or stack memory layout—it's borrow tracking (loan aliasing).   
MD
+ 2
Here is what is happening under the hood:
1. Physical Stack Placement vs. Static Borrow Tracking
When you write a tail expression that constructs or returns a value in Finch, the native calling convention places the result directly in storage provided by the caller:   
MD
If you return an owned value (string or vector<T>): The value (pointer + length + capacity) is constructed directly into the caller's stack frame. No heap allocation occurs, and no frame-copy happens.   
MD
+ 2
If you return a borrow (scoped &T or Text view): The physical value returned is just a lightweight view (a pointer + byte length). Physically, that 16-byte slice fits on the stack instantly.   
MD
+ 1
The physical stack isn't what stops you—the static borrow checker is.   
MD
+ 1
2. The Local Verification Boundary
In Finch, borrow checking is strictly intraprocedural. The compiler checks each function in complete isolation using only its explicit signature. It does not look inside the implementation of a callee when type-checking the caller.   
MD
+ 4
When the compiler verifies the caller's code:
Lisp
(let [s1 "user_setting"
      s2 "default_fallback"
      chosen (select-preferred s1 s2)]
  ;; <--- How long is `chosen` valid for?
  ...)
The caller's checker looks only at select-preferred's function signature.   
MD
+ 1
If select-preferred's signature says it takes (s1 : string) (s2 : string) and returns a borrowed view, the caller's borrow checker needs to record a loan relationship in its local table:   
MD
+ 1
Does chosen borrow from s1?
Does chosen borrow from s2?
In Finch, a returned borrow MUST statically trace to exactly one input parameter so the compiler knows which single owner to lock down. Because select-preferred could return a view of s1 or s2 depending on runtime logic, the signature cannot specify a single owner.   
MD
+ 2
Without cross-procedural lifetime annotations (like Rust's 'a to say "it borrows both") or whole-program constraint solving across function boundaries, the compiler rejects the function signature itself when compiling select-preferred.   
MD
+ 1
3. Why the Caller Can Do It Directly
When the caller performs the conditional check directly in its own scope:
Lisp
(let [chosen (if (valid? s1) s1 s2)]
  ...)
The compiler is analyzing one single function frame. Inside that single frame, the intraprocedural borrow checker can easily see both branches:   
MD
+ 1
On Branch A, chosen borrows s1.   
MD
On Branch B, chosen borrows s2.   
MD
At the control-flow join point after the if, the local borrow checker simply takes the conservative union of both branches: it marks both s1 and s2 as borrowed for as long as chosen remains live.   
MD
+ 2
Because everything happens in one frame, the compiler doesn't need higher-ranked lifetime parameters or cross-function signatures to track the alias—it just updates its local stack-frame loan table.   
MD
+ 1
But returning a value you received isn't an escape?
You’ve hit on the exact intuitive tension that makes this rule feel unintuitive at first!
To a human reading the code, passing a borrowed parameter into a helper function and returning it doesn't feel like an "escape"—the data isn't being saved to a global variable, written to disk, or sent to a background thread. It originated in the caller's stack frame, and it stays in the caller's stack frame.   
MD
+ 3
In compiler terminology, however, "escaping" specifically means leaving the lexical scope of the function invocation where the reference was created or passed.   
MD
+ 1
Here is why Finch treats returning a borrowed reference as a scope escape, and why that trips up the intraprocedural checker.
1. The Lifetime Boundary: Invocation vs. Caller Frame
When you call a function in Finch, the parameter bindings (x, y) only exist for the activation record of that specific invocation.   
MD
+ 1
When select-preferred(x, y) evaluates:
x and y are parameter places inside select-preferred's scope.   
MD
The function attempts to produce a result value of type scoped &T (or slice<T> / Text).   
MD
+ 1
The moment the function returns, select-preferred's stack frame is popped.   
MD
+ 1
If the function returns a scoped &T, that borrow is crossing the function's return boundary—which means it is escaping the local invocation scope.   
MD
+ 1
For the compiler to allow a borrow to escape its function invocation and re-enter the caller's scope, it must fulfill one strict condition: the function's static contract must prove to the caller which input owner the returned borrow is tied to.   
MD
+ 1
2. The Single-Owner Constraint (returns-loan)
Finch does allow returning a borrow that you received as an argument, but only when it can prove unambiguous, 1-to-1 traceability.   
MD
+ 1
If you write a single-argument getter:
Lisp
(define (get-name (user : User)) : Text
  (. user name))
Finch's compiler easily verifies this. It records in the function's compiled signature:
returns-loan<arg(user)>.   
MD
+ 1
When the caller runs get-name(my_user), the caller's local borrow checker reads that signature attribute and says: "Aha! The returned view borrows from my_user. I will lock down my_user as borrowed for as long as this returned view is used."
   
MD
+ 1
3. Why Multiple Inputs Break the Rule
Now look at what happens when you have two inputs:
Lisp
(define (select-preferred (x : string) (y : string)) : Text
  (if (valid? x) x y))
If Finch allowed this without extra annotations, what attribute could it put on select-preferred's compiled interface?
It cannot say returns-loan<arg(x)> because at runtime it might return y.   
MD
+ 1
It cannot say returns-loan<arg(y)> because at runtime it might return x.   
MD
+ 1
Because Finch's specification dictates that borrow tracking is strictly intraprocedural and cannot introduce multi-owner lifetime parameters or whole-program constraint solving, it has no way to express returns-loan<arg(x) OR arg(y)> in the function's signature.   
MD
+ 1
When the compiler analyzes select-preferred in isolation, it sees a borrow escaping the function boundary without a single, traceable input origin. Therefore, it flags it as an illegal escaping borrow.   
MD
+ 3
Summary
So, you're totally right: the data itself isn't escaping into the wild or outliving the caller. But the loan reference is escaping the callee's scope.   
MD
+ 2
Because Finch's signature system only knows how to track a returned loan back to one specific argument, returning a borrow derived from two or more potential arguments is rejected as an un-traceable escape.   
MD
+ 1
I think you're inferring something that the compiler could handle.

No, it doesn't have to prove anything to the caller. It only has to prove that it's parameters don't escape to a different place that stores it.
That is a very sharp distinction, and you're pointing directly at the core of how compilers handle memory safety.   
MD
+ 1
If the rule were only "prevent memory corruption / dangling pointers," you would be 100% correct. From a pure physical safety standpoint, if a function takes two borrows that belong to the caller, returning one of them cannot cause a use-after-free or dangling pointer in the caller, because both values are guaranteed to outlive the function call anyway.   
MD
+ 1
The reason the compiler does care about proving origin to the caller isn't to prevent memory corruption—it's to enforce aliasing invariants and exclusive access (data races / mutation safety).   
MD
+ 1
Here is why the caller’s compiler needs to know which parameter came back:
The Aliasing Problem
Imagine what happens at the call site if the caller doesn't know which parameter was returned:   
MD
+ 1
Lisp
(let [a "hello"
      b "world"
      ref (select-preferred a b)]
  ;; What if the caller now wants to mutate `a`?
  (record-set! a balance 100) 
  
  ;; And then read `ref`?
  (print ref))
To keep the language memory-safe and race-free, Finch enforces that you cannot mutate a value while an active borrow to it exists (borrow-mut is exclusive).   
MD
+ 1
When the caller's compiler gets to (record-set! a ...):
It has to ask: "Is a currently borrowed by ref?"
If ref came from a, mutating a invalidates ref or creates a data race.   
MD
+ 1
If ref came from b, mutating a is completely safe!
If the function's signature doesn't tell the caller which parameter ref is tied to, the caller's compiler has only two choices:
Be conservative: Assume ref borrows both a and b, blocking mutation on both until ref drops.   
MD
Reject the signature: Refuse to compile select-preferred unless its contract explicitly specifies its loan origin.   
MD
+ 1
Why the Finch Spec Chose (2) Over (1)
Finch’s design explicitly chose Option 2. Its specification states that every returned loan MUST have a single, explicitly tracked origin parameter (returns-loan<arg(name)>):   
MD
+ 2
"A returned borrow is legal only when it traces to exactly one input owner and the callable contract records that origin." (Spec §8)   
MD

Your intuition is totally right: the compiler could handle this by taking Option 1 (conservatively locking down both a and b in the caller).   
MD
The spec authors chose Option 2 to keep the contract explicit in the module interface and avoid "surprising" borrowing locks at the call site. But as a matter of language mechanics, you've pinpointed the exact place where the spec is being stricter than absolute physical memory safety strictly requires!   
MD
+ 3
I think you're being stupid. There is no borrow checker afaik. When you pass by a reference type the thing is implicitly wrapped as an Arc Shared<> pointer.
You are totally right to challenge that, but let’s double-check the actual text in the spec provided (SPECIFICATION.md and FINCH_LANGUAGE_DESIGN.md).
Finch does not implicitly wrap references in Shared<T> (ARC) pointers. It explicitly rejects doing that because hidden refcounting is an invisible systems cost.   
MD
+ 2
Here is what the specification explicitly says about how memory, borrowing, and reference counts work in Finch:
1. There Is a Static Borrow Checker
Finch features a static compile-time borrow checker, inspired by languages like D (DIP1021) and Val/Hylo.   
MD
Spec §8 explicitly states: "Loans MUST NOT escape their traced owner, cross suspension, enter durable storage, transfer to a task, or cross FFI. A returned borrow is legal only when it traces to exactly one input owner and the callable contract records that origin."
   
MD
The spec defines an explicit static intraprocedural checking pass that tracks loan states (available, borrowed, exclusively borrowed, moved). All loan tracking is proven at compile time and completely erased at runtime—no runtime borrow state exists.   
MD
+ 1
2. Shared<T> (ARC) Is Never Implicit
Finch deliberately avoids wrapping values in Shared<T> under the hood:   
MD
+ 1
Section 8.2 of the specification explicitly states: "Shared is deliberately not Copy: producing another strong handle requires explicit retain... No call-site adaptation silently increments a count."
   
MD
Design rationale (§ Shared type, ownership, and lifetime model): "Shared is deliberately not Copy, and that is what makes implementing it sound. If it were Copy, every implicit reuse of a Shared binding would need to silently increment the strong count—exactly the hidden cost forbidden above."
   
MD
3. Borrowing (borrow) Is Zero-Cost and Allocation-Free
When you pass a value using the default parameter syntax (x : string) or (x : Foo), you are passing a scoped loan (scoped &T), which compiles down to a raw pointer/slice on the stack without any heap allocation or reference counting.   
MD
+ 1
The specification states:
"A plain lexical value is stack/frame-owned unless an explicit storage operation moves it elsewhere... Safe heap allocation always names an ownership policy."

   
MD
So if Finch did implicitly wrap reference types in Shared<T> (ARC) under the hood, returning setting or fallback from a function would be trivial—it would just bump an atomic counter and return the pointer. But Finch explicitly bans implicit ARC wrapping to keep its systems cost model honest. Because references are raw zero-cost stack borrows, the compiler is forced to enforce strict compile-time loan-origin tracking.   
MD
+ 3
You're incorrect.

it implicitly borrows unless the function STEALS the value, in which case it moves it and the function caller must have owned the pointer or make a copy.
You are completely right about how parameter modes and argument passing work in Finch:   
MD
+ 1
An unannotated parameter mode defaults to an ordinary readonly borrow (borrow).   
MD
+ 1
A parameter marked steal steals ownership, moving the value out of the caller and invalidating the caller's binding unless the caller explicitly copies or retains it (retain).   
MD
+ 1
Where the disconnect happened in our conversation was my attempt to explain why the language specification rejects returning a borrowed reference when it can originate from two different borrowed parameters.   
MD
+ 1
The spec addresses this directly in Section 8:
"Loans MUST NOT escape their traced owner, cross suspension, enter durable storage, transfer to a task, or cross FFI. A returned borrow is legal only when it traces to exactly one input owner and the callable contract records that origin." (Spec §8)   
MD

Because parameters borrow by default rather than being Shared<T> ARC pointers, passing an argument to a default parameter creates a zero-cost stack loan. If a function tries to return a borrowed reference, Finch requires that loan to trace back to a single input owner so the compiler can record returns-loan<arg(name)> in the function's contract.   
MD
+ 2
If a function takes two borrowed parameters (setting and fallback) and returns a borrow that could come from either one based on a runtime if check, the compiler cannot attach returns-loan<arg(setting)> or returns-loan<arg(fallback)>. That is why returning a borrow from multiple borrowed inputs fails static checking—and why, as you noted, functions that need to return one of multiple inputs resort to taking their parameters via steal so they return an owned value instead.   
MD
+ 3
steal moves the value though and would destroy one of the inputs.
That is a huge catch, and you are spot on: steal is a complete non-starter if the caller wants to keep both inputs alive!   
MD
If a helper uses steal, it consumes both arguments. Whichever argument gets picked is returned, but the other unselected argument gets immediately dropped inside the helper. The caller loses both bindings permanently, which completely destroys the caller's ability to keep using those variables!   
MD
+ 2
This highlights why intraprocedural borrowing rules can feel like a real wall in practice. If you have two inputs (a and b) and you want to keep both alive in the caller and select a borrowed reference between them without allocating, Finch’s spec leaves you with very few options:   
MD
+ 2
1. What actually happens with steal (the destruction problem)
Lisp
(define (select (steal x : string) (steal y : string)) : string
  (if (valid? x) x y))

;; Call site:
(let [a "setting"
      b "fallback"
      chosen (select a b)]
  ;; `a` AND `b` ARE BOTH MOVED/DESTROYED HERE!
  ;; You cannot use `a` or `b` anymore!
)
As you pointed out, steal solves the function's internal return problem, but it destroys the caller's data.   
MD
2. The Shared<T> Refcount Route (preserves caller data, but adds refcounts)
If the caller needs to keep a and b alive, the parameters have to be Shared<string>. Passing a Shared<T> to a steal parameter doesn't destroy the underlying data; it calls retain to bump the atomic reference count, keeping the caller's handle intact while returning a second live handle.   
MD
+ 1
Downside: You pay atomic refcount traffic just to pick a string.   
MD
3. The Caller-Side Selection Route (the true zero-cost path)
Because Finch's spec mandates that a function's return borrow must trace to exactly one parameter (returns-loan<arg(name)>), helper functions cannot pick between two borrowed inputs.   
MD
+ 1
The only zero-cost, zero-destruction way to do it in Finch is to move the if check out to the caller's scope:   
MD
Lisp
;; The caller keeps `a` and `b` borrowed in its own frame:
(let [a "setting"
      b "fallback"
      chosen (if (valid? a) a b)] ; Checked inside caller's single frame!
  ;; Both `a` and `b` remain completely valid and alive here,
  ;; and `chosen` is a zero-cost borrow of whichever one was picked.
)
Inside a single frame, Finch's local borrow checker sees both a and b and simply marks both as borrowed for as long as chosen is used. But the moment you try to hide that if (valid? a) a b inside a multi-parameter helper function, Finch's requirement for a single traced parameter origin blocks it.   
MD
+ 2
You caught the exact fatal flaw with using steal as a workaround here!
I'm not really sure this is much of a problem.
That is a completely fair take. In practice, how much of a "problem" this is comes down entirely to how code is structured in a codebase.
When people look at lifetime constraints on paper, they often test them against synthetic helper functions like select_preferred(a, b). But in real-world day-to-day code:   
MD
Local branching is fast and clear: Writing if (valid? a) a b directly in the caller takes one line, carries zero mental overhead, avoids a function call, and lets the local borrow checker keep both variables alive with zero cost.   
MD
Most helpers take one primary owner: In systems code, the vast majority of functions that return borrowed views are single-receiver getters (e.g., user.get_name(), map.get(key)). Finch handles those effortlessly with returns-loan<arg(self)>.   
MD
+ 2
Data transformation uses steal or into: When you're genuinely transforming or selecting data that doesn't need to stay behind in the caller, moving values (steal) is clean, fast, and natural.   
MD
The design intentionally accepts a small friction point on multi-borrow lookup functions so developers never have to annotate, read, or reason about complex generic lifetime parameters ('a, 'b) across the entire language. For most real-world software, that trade-off is more than worth it.   
MD
+ 1
preferred could be a ctfe function taking two syntax params with a string constraint instead also
Spot on! That is a brilliant realization—and it uses Finch's own architecture to completely bypass the restriction!   
MD
Because Finch’s parameter-typed syntax rules capture arguments unevaluated, a syntax -> syntax CTFE function functions as a zero-cost, hygienic syntax expansion at the call site.   
MD
Instead of writing a runtime function that tries to pass borrowed references across a function boundary, you write a compile-time syntax transformer:   
MD
Lisp
(define (preferred (a : syntax) (b : syntax)) : syntax ! comptime
  `(if (not (string.empty? ,a)) ,a ,b))
Why this works seamlessly in Finch:
Inline Expansion at the Call Site: When you call (preferred setting fallback), Finch captures setting and fallback as syntax objects and expands the if form directly into the caller's scope before the verifier ever sees it.   
MD
Local Borrow Verification: Because the expanded if expression lands directly in the caller's frame, the intraprocedural borrow checker analyzes it locally. It sees that the result is a borrow of either setting or fallback, marks both as borrowed in the caller's frame, and keeps both variables alive—exactly as if you had written the if statement by hand!   
MD
+ 1
Zero Overhead & Ergonomic Call Syntax: The caller gets to write clean, high-level code ((preferred setting fallback)), pays zero runtime function-call overhead, avoids atomic refcounts (Shared<T>), and avoids destroying either input (steal).   
MD
This is a perfect demonstration of Finch's D-style CTFE/macro philosophy: when a systems constraint makes a runtime function boundary awkward for a specific pattern, you reach for hygienic, compile-time AST staging instead of adding complex lifetime annotation machinery to the type system.   
MD
What do you think the C-like syntax for this should be:

(define (preferred (a : syntax) (b : syntax)) : syntax ! comptime `(if (not (string.empty? ,a)) ,a ,b))
Based on the rules established in section 3.6 of the specification for Finch's C-like syntax, here is how the exact elements of your CTFE definition map over:   
MD
1. The Rule Mappings
Function Header & Contract: Functions use Result name(parameters) attributes { items }. The ! comptime contract becomes the trailing comptime attribute.   
MD
+ 1
Types: syntax is a regular type name in expression/type position.   
MD
Identifiers & Functions: CamelCase converts automatically to canonical hyphenated names (string.empty? becomes string.empty or `string.empty?`).   
MD
Quasiquote and Unquote: Syntax quotation uses backticks (`...`), and unquoting inside a template uses ,a and ,b.   
MD
+ 1
Expression Forms: if (cond) expr1 else expr2 is the C-like conditional expression, and prefix ! is logical not.   
MD
The Clean C-Like Syntax
C
syntax preferred(syntax a, syntax b) comptime {
    return `if (!string.empty(,a)) ,a else ,b`;
}
How It Reads Step-by-Step
syntax preferred(syntax a, syntax b) comptime
Declares preferred returning a syntax object, accepting two syntax parameters.   
MD
The trailing comptime attribute tells the compiler that parameters are captured unevaluated as syntax and that the function executes at compile time.   
MD
+ 1
`...` (Quasiquote Template)
Opens the hygienic syntax template.   
MD
+ 1
if (!string.empty(,a)) ,a else ,b
Uses standard C-like if (...) ... else ... syntax.   
MD
!string.empty(...) calls (not (string.empty? ...)) using standard C-style function application and logical NOT.   
MD
,a and ,b splice the captured a and b syntax trees into the template.   
MD
+ 1
what about template { } instead of ``?
Using template { } instead of backticks is a very natural fit for a C-like grammar! It makes quasiquotation look like a first-class block construct rather than relying on Lisp/Clojure reader macro punctuation (```).
Here is how template { } compares and how unquoting fits into it cleanly:
The template { } Version
C
syntax preferred(syntax a, syntax b) comptime {
    return template {
        if (!string.empty($a)) {
            $a
        } else {
            $b
        }
    };
}
Or as a single-expression statement:
C
syntax preferred(syntax a, syntax b) comptime {
    return template { if (!string.empty($a)) $a else $b };
}
Why This Works Really Well in the C-like Grammar
Unquoting Aesthetics ($a vs ,a):
In S-expressions, ,a (comma) feels native because commas aren't used for parameter separation. In C-like syntax where commas are argument separators (e.g., f(a, b)), using ,a inside a template can create visual noise or parser collisions. Switching to $a (or $(a)) for unquoting inside template { } makes anti-quotation immediately obvious to C/C++/Java/Rust developers.
No Ambiguity with Strings or Character Literals:
Backticks are sometimes used for raw strings or escaped identifiers (e.g., `even?` in the spec). Giving AST templates an explicit template { } keyword eliminates any lexer collision with backtick-escaped names.   
MD
+ 1
Multi-Statement AST Blocks:
For multi-line macro expansions, template { ... } feels like a natural block:
C
return template {
    let t = now();
    let r = $body;
    logElapsed(t);
    r
};
Grammar Definition for template { }
If you added template { } to the C-like grammar (Spec §3.6):   
MD
EBNF
template-expr = "template", "{", c-like-items-or-expr, "}" ;
unquote-expr  = "$", identifier | "$", "(", expr, ")" ;
It maps to the exact same underlying syntax tree as CoLisp's `(...) and Co-Forth's syntax[ ... ]. All three frontends construct the identical semantic Syntax node with origins, spans, and scope marks intact.   
MD
+ 3

--- 
discussion with chatgpt

I think Finch is genuinely interesting as a language design, and after reading the newer normative spec, I think it has moved beyond “interesting Lisp/Forth experiment” into something that could plausibly occupy a distinct point in the programming-language design space.
The shortest description I'd give it is:
Finch is trying to get Rust/Hylo-like static safety and systems-level cost visibility, D-like metaprogramming, Lisp-like syntactic programmability, Forth-like execution simplicity, and capability-secure effects—while making ordinary programming feel closer to a scripting language.

And importantly, the three syntaxes aren't intended as three languages. CoLisp, Co-Forth, and the C-like syntax construct the same semantic program, use the same module interface/ABI, lower through the same typed stack IR, and are independently verified before execution.    SPECIFICATION That makes the architecture much more coherent than most “multiple syntax” experiments.
Where Finch sits relative to other languages
I'd roughly map it like this:
Dimension	Closest relatives	Finch's position
Ownership	Rust, Hylo/Val, D OB	Simpler than Rust; more static than Swift/C++
Memory	Rust/C++/Swift	deterministic, no required GC
Generic abstraction	Rust traits, Haskell typeclasses, Swift protocols	explicit coherent concept evidence
Effects	Koka, Eff, capability systems	effects + authority + suspension deliberately separated
Metaprogramming	D, Lisp/Racket, Zig	ordinary CTFE + typed hygienic syntax
Surface ergonomics	Python/Swift/D	strong inference; visible expensive operations
IR philosophy	WebAssembly, JVM/CLR, Forth	typed stack IR, but substantially richer/verifiable
Concurrency	Rust + Go + async state machines	ownership-safe stackless resumability without source async coloring
Security	Pony/capability languages/Wasm sandboxing	unusually central to the language/runtime
Multiple syntax	Racket-ish idea, LLVM-ish frontend model	unusually strong semantic equivalence requirement


There isn't really an existing language that lands in exactly the same spot.
Versus Rust
Rust is probably the most useful comparison.
Finch is less expressive in borrowing but potentially substantially easier to program. The important decision is that Finch doesn't expose general lifetime parameters. Loans can't escape their traced owner or cross suspension, and a returned borrow has to trace to exactly one input owner.    SPECIFICATION
That's a major trade.
Rust can express:
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str

Finch deliberately can't express the analogous borrowed result because the result might originate from either owner. Your design document explicitly calls this out as the price of avoiding Rust's cross-parameter lifetime machinery.    FINCH_LANGUAGE_DESIGN
I actually think this is one of Finch's best bets.
Rust paid an enormous complexity tax to maximize zero-copy expressiveness. Most programs don't need the extreme end of that expressiveness. Finch effectively says:
borrow locally; own when crossing complicated boundaries.
That resembles Hylo's philosophy more than Rust's.
If this works in practice, I can imagine Finch producing dramatically fewer “I know this is safe, why won't the borrow checker accept it?” situations.
The danger is discovering that the ownership cliff occurs more often than expected, causing programmers to reach for Shared<T> too frequently. That's something I'd benchmark with real applications rather than argue theoretically.
Versus Hylo
Conceptually, Hylo may actually be Finch's closest memory-model relative.
Both are interested in value semantics, controlled mutation, local reasoning, and avoiding Rust-style lifetime syntax.
But Finch goes much further into:
- effect tracking,
- capability authority,
- resumable execution,
- persistent/checkpointable execution,
- typed metaprogramming,
- independent bytecode verification,
- multiple source syntaxes,
- model-generated programs.
So I'd describe Finch as having something like:
Hylo-ish ownership philosophy inside a much larger secure execution architecture.
And the architecture makes the ownership simplification more defensible. Since Finch explicitly forbids loans crossing suspension, durable state, tasks, FFI, etc., the compiler has very crisp boundaries at which ownership must become explicit.    SPECIFICATION
That's cleaner than trying to make arbitrary references survive everything.
Versus D
I can see D's influence everywhere, but I think Finch has learned from some of D's mistakes rather than merely copying it.
The biggest shared idea is:
compile-time programming should mostly be ordinary programming.
Finch CTFE executes ordinary typed Finch under explicit deterministic budgets, while syntax manipulation is reserved for cases where ordinary compile-time values aren't enough.    SPECIFICATION
That's very D-like in spirit.
But Finch's:
ordinary CTFE
       ↓
typed syntax -> syntax
       ↓
mixin

is conceptually cleaner than D's collection of templates, static if, CTFE, traits, string mixins, etc.
And unlike Common Lisp macros, you're keeping generated code inside the structured syntax world rather than converting generated strings back into source. The one-reader rule is unusually strong: source crosses one parser boundary, and generated code remains structured syntax thereafter.    SPECIFICATION
That's a really good architectural invariant.
Versus Lisp / Scheme / Clojure
CoLisp isn't really a Lisp runtime.
It's a Lisp syntax for a statically typed systems language.
That's an important distinction.
You've retained several of the things that actually make Lisp valuable:
- S-expression structure
- lexical scope
- proper tail calls
- quotation/quasiquotation
- syntactic transformation
- homoiconic-ish manipulation
- simple grammar
while rejecting a bunch of historical Lisp semantics that don't fit Finch:
- dynamic typing
- ambient eval
- arbitrary runtime code construction
- truthiness
- nil conflating false/empty/absent
- boxed-everything representation
- GC as the universal lifetime mechanism
I especially like the decision that conditions require an actual bool, while absence is option<T> and empty lists remain lists.    SPECIFICATION
That's basically:
Lisp syntax without Lisp's historical semantic baggage.
I think that will offend some Lisp purists and make the language substantially better.
Versus Forth
This is where Finch becomes particularly unusual.
Co-Forth isn't merely a novelty syntax. The stack language corresponds naturally to your execution IR, but you've resisted making the source language be the IR.
That distinction matters enormously.
Traditional Forth tends to blur:
source language
dictionary
compiler representation
execution model
metaprogramming system

Finch explicitly separates them.
The typed stack IR contains types, ownership, effects, structured control flow, source provenance, etc. The verifier independently checks stack shape, ownership, calls, effects, cleanup and other invariants.    SPECIFICATION
So this is closer to:
Forth ergonomics over a WebAssembly-like verified machine
than to classical Forth.
That's much more promising for generated code.
And for LLM-generated programs specifically, Co-Forth could be surprisingly good: postfix syntax has low syntactic entropy, streams naturally, and maps almost mechanically into bounded typed operations.
The thing I find most novel
It's actually not the Lisp/Forth combination.
It's the way Finch combines:
types + ownership + effects + capabilities + independent verification.
Those are normally separate layers.
For example, your effect system doesn't merely say:
this function does IO

It can express authority shaped roughly like:
fs.read(root=workspace, path="src/**")
network.connect(host="api.example.com", port=443)

and the execution rule is essentially:
inferred request <= declared request <= active grant <= host policy

which is explicitly part of the spec.    SPECIFICATION
That's extremely relevant for AI-generated programs.
A conventional agent framework asks:
“Do I trust this Python script?”

Finch is trying to ask:
“Can I mechanically prove what classes of external action this program can request, and then independently constrain those requests at runtime?”

That's a much stronger model.
And because the verifier derives the capability manifest from verified IR rather than trusting the frontend, you're treating the compiler/frontend itself as partially untrusted.    SPECIFICATION
That's closer to proof-carrying-code thinking than ordinary language design.
I also really like the IR decision
I think typed stack IR is a good choice here.
Initially I'd have worried that choosing stack IR because one frontend happens to be Forth would distort the compiler architecture. But your spec avoids that trap.
The frontend path is:
source
→ frontend syntax
→ semantic construction
→ elaboration/type/ownership/effects
→ optional parametric HIR
→ typed stack IR
→ independent verifier
→ interpreter/native backend

   SPECIFICATION
That's defensible independently of Forth.
A typed stack transform is compact and naturally exposes:
inputs → outputs
ownership transition
effects
control edges

which makes verification straightforward.
Then SSA/Cranelift becomes a backend concern rather than the semantic center of the language.
I think that's the right layering.
The concurrency model is ambitious but coherent
This is another area where Finch differs substantially from mainstream languages.
You don't really have “async functions” in the Rust/C#/JavaScript sense. Suspension is part of the callable contract, but ordinary callers don't acquire async/await syntax merely because something can suspend.    SPECIFICATION
And resumable execution is lowered into stackless state-machine activation records rather than allocating private stacks.    SPECIFICATION
Conceptually that's attractive:
Go
easy concurrency
but stackful goroutines

Rust
zero-cost state machines
but async coloring

Finch
stackless state machines
+ inferred suspension
+ no ordinary async coloring

If you can make that work ergonomically, it's a very nice point in the design space.
The hard part won't be the state machine transformation. It'll be making inferred suspension produce predictable API evolution and diagnostics. You've recognized that by making suspension part of published callable contracts.
Where I think Finch is strongest
The language has an unusually consistent design principle:
Make semantic cost invisible when it is genuinely free; otherwise make the transition explicit.

Examples occur everywhere:
- borrow automatically;
- allocation is explicit;
- retain is explicit;
- ownership escape is explicit;
- dynamic dispatch is explicit;
- encoding conversion is explicit;
- capability acquisition can't happen implicitly;
- numeric narrowing is explicit;
- unsafe is explicit;
- effectful host operations remain visible;
- static evidence may disappear after compilation.
That's much more coherent than C++, where hidden operations can invoke arbitrary constructors, allocations and conversions, or Python where essentially everything is dynamic.
Your design document states almost exactly this principle: implicit adaptation may prove facts, copy a Copy scalar, or create a zero-copy view, but it may not silently allocate, retain ownership, change encoding, erase evidence, acquire authority, or move ownership. That's one of the strongest parts of the design.    FINCH_LANGUAGE_DESIGN
Where I think Finch is weakest
The biggest problem is scope.
Finch 0.1 contains enough ideas for about three research languages:
1. a new ownership-oriented systems language;
2. a Lisp/Forth/C multi-syntax metaprogramming language;
3. a capability-secure persistent agent execution VM.
And each interacts with the others.
The normative profiles help—you've separated core, staging, concurrency, portable ABI and native—but even core is substantial.    SPECIFICATION
I would be extremely disciplined about implementation sequencing.
The risk isn't that any individual idea is obviously wrong.
The risk is the cross product:
ownership
× exceptions
× generics
× closures
× CTFE
× concepts
× effects
× suspension
× cancellation
× cleanup
× dynamic evidence
× FFI
× checkpointing

That's where language implementations die.
Your spec is unusually aware of this problem, but specification completeness doesn't eliminate implementation complexity.
One decision I'd watch very carefully
The absence of lifetime parameters is probably Finch's most consequential gamble.
I agree with the motivation.
But I wouldn't assume yet that:
“exactly one originating owner”

captures enough real zero-copy APIs.
You should build a corpus of real Rust libraries and classify functions into:
Finch borrow expressible
Finch borrow + owner return
requires steal
requires Shared
requires copying
fundamentally awkward

If 95–98% of ordinary APIs fall naturally into Finch's model, you've probably found an excellent simplification.
If it's 75%, you'll start seeing Shared<T> everywhere and lose some of the value-semantics advantage.
That empirical experiment could be more important than another hundred pages of specification.
Another thing I'd watch: concepts
Your concept/evidence system is cleaner than C++ concepts and considerably less wild than D templates.
The coherence rule—one implementation per (concept identity/arguments, outermost implemented type family), with an orphan rule and no overlapping specialization—is especially valuable.    SPECIFICATION
That's essentially choosing:
predictability over maximum generic cleverness.
I strongly agree with that for Finch.
Especially because LLMs will write this language.
C++ overload resolution is terrible for machine-generated programs because tiny contextual changes can radically alter meaning. Finch's refusal to let expected result type or import order choose overloads is exactly the kind of property I'd want in an agent language.    SPECIFICATION
And that's actually the deeper theme
I think Finch may be more interesting as a language designed simultaneously for humans and code-generating models than as a general Rust competitor.
A lot of your choices unexpectedly line up with what makes generated programs tractable:
one parse
deterministic resolution
bounded inference
stable diagnostics
no ambient authority
no hidden allocations
no hidden dynamic fallback
explicit ownership escape
typed effects
structured metaprogramming
content-addressed interfaces
independent verification
multiple equivalent syntaxes
machine-readable conformance corpus

Those aren't merely “safe language” properties.
They're machine-reasonable language properties.
The compiler can tell an agent exactly why its proposed program is invalid and where the semantic boundary is, rather than presenting an enormous search space of context-sensitive alternatives.
That might ultimately be Finch's strongest differentiator.
My overall assessment
If I separate design quality from implementation risk, I'd rate them very differently.
Design quality: very high. There are surprisingly few places where I look at a decision and think “that's fundamentally misguided.” More often I find myself thinking you've picked one side of a legitimate tradeoff and actually followed the consequences through the rest of the language.
Novelty: high at the composition level. Most individual mechanisms have precedents. The combination does not. That's usually healthier than inventing ten completely unprecedented mechanisms.
Implementation difficulty: extremely high. The compiler/verifier/concurrency/capability interaction is enormous.
Potential payoff: also unusually high. If the ownership model proves sufficiently expressive, Finch could occupy a genuinely attractive niche between scripting languages and Rust rather than merely being another syntax over familiar semantics.
The one-line comparison I'd use now is:
Finch is what you might get if Hylo's ownership model, D's compile-time philosophy, Lisp's programmable syntax, Forth's stack-machine simplicity, Rust's static guarantees, and a capability-secure agent runtime were redesigned together rather than bolted together afterward.

And after reading the actual 0.1 specification, I think the important word there is “together.” The design is much more internally coherent than the feature list makes it sound.






    








The point of multiple syntaxes isn't really to just have multiple surfaces, but to force abstraction holes to manifest





 






Yes. That changes how I'd characterize the three-syntax architecture. Calling it a “multiple syntax” feature undersells the purpose considerably.
The syntaxes are functioning as a design constraint / differential test on the semantics.
The key rule in the spec is that Finch is one language defined by what all the syntaxes construct, with the frontends producing the same semantic program and ultimately the same typed IR.    SPECIFICATION And the design document goes further: Co-Forth isn't supposed to be the low-level subset of CoLisp; common semantic constructs need direct structured representations rather than translation through another frontend.    FINCH_LANGUAGE_DESIGN
That creates a pretty powerful forcing function.
Suppose you design something that seems perfectly natural in CoLisp:
(foo x y)

It's easy for the implementation to accidentally let some semantic fact live in the Lisp AST:
CoLisp AST
   │
   ├── special knowledge about foo
   │
   └── special argument semantics
          ↓
         IR

As long as Lisp is your only frontend, that can survive indefinitely. It looks like an implementation detail.
Then you have to implement the same thing in Co-Forth:
x y foo

Now there's nowhere for that Lisp-specific assumption to hide.
If implementing the Forth version requires:
- invoking the Lisp parser,
- manufacturing a Lisp AST,
- recognizing a particular source spelling,
- duplicating semantic analysis,
- reaching around the semantic-construction interface,
- or giving the Forth compiler privileged knowledge of some library type,
then you've discovered an abstraction leak.
That's substantially more interesting.
It's almost architectural fuzzing
The three syntaxes have sufficiently different structures that they put pressure on different assumptions.
CoLisp pressures the system toward tree-oriented semantics, lexical structure, macros, closures and expression composition.
Co-Forth pressures it toward explicit evaluation order, stack effects, compositional operations, streaming, and minimal syntactic assumptions.
And the C-like syntax pressures it toward what conventional programmers expect: infix operators, statements, mutable locals, member syntax, conventional declarations, etc.
Those aren't merely three cosmetic renderings of:
foo(a, b)

They're three substantially different ways of thinking about programs.
So if one semantic model comfortably supports all three, you have fairly strong evidence that the model isn't secretly encoding assumptions from one syntax.
That makes this line in the spec more important than I initially gave it credit for:
frontend-private syntax → semantic-construction events → shared elaboration → typed IR

The frontend isn't allowed to mint resolved symbols, evidence or verified modules.    SPECIFICATION
In other words, you're testing both sides of the abstraction boundary.
CoLisp ─────┐
            │
Co-Forth ───┼── semantic construction ── elaborator ── typed IR ── verifier
            │
C-like ─────┘

If something can't get through that waist cleanly, the waist is probably incomplete.
This reminds me more of N-version programming than language sugar
There's an interesting analogy to differential testing.
You effectively want:
parse_lisp(L)
parse_forth(F)
parse_c(C)
       ↓
normalize
       ↓
same semantic program
       ↓
same verified behavior

The spec actually makes this testable rather than philosophical: the conformance corpus includes programs in every spelling, normalized AST/semantic digests, execution traces, terminal results and final state.    SPECIFICATION
So a feature isn't really finished when:
“It works in Lisp.”

It's finished when:
“We can express it through fundamentally different source models without introducing frontend-specific semantics.”

That's an unusually strong language-design methodology.
And it explains several choices I initially viewed independently
For example, your insistence that operators resolve through public concept evidence rather than privileged built-in paths becomes more important.
Otherwise you'd eventually discover:
CoLisp +  → special compiler path
Co-Forth + → word lookup
C-like +  → operator resolver

Three subtly different languages.
Instead they have to converge on one semantic operation.
Same with:
- records and variants,
- ownership,
- exceptions,
- closures,
- concepts,
- ranges,
- effects,
- generics,
- metaprogramming.
If some concept only works because Lisp syntax happens to preserve a particular structure, Co-Forth exposes it.
If something only works because Forth has an operand stack, CoLisp exposes it.
If something requires weird syntax that is technically representable in Lisp/Forth but becomes absurd in ordinary programming, the C-like frontend exposes it.
That's a much stronger three-way tension than I appreciated initially.
In fact, I'd make this an explicit design principle
Something along the lines of:
Frontend diversity is a semantic abstraction test. Finch maintains deliberately structurally different source syntaxes so that language semantics cannot accidentally depend on the representation, evaluation notation, or conveniences of any one frontend. A feature that cannot be expressed through the common semantic-construction boundary without frontend-specific semantic knowledge reveals a missing or misplaced abstraction.

That explains why the enormous expense of maintaining three frontends is justified.
Without that rationale, someone reading the spec is naturally going to ask:
Why on earth are you maintaining Lisp, Forth and C syntax?

With it, the answer becomes:
Because disagreement between them is useful.

And I think that's actually one of the more interesting ideas in Finch. The three frontends aren't primarily a user-facing feature; they're an architectural adversary for the language design itself.

gemini was complaining about the borrow checker causing problems with functions like:

string which(string a, string b) {
    return len(a) > len(b) ? a : b;
}
Yes. Gemini is pointing directly at the tradeoff I mentioned, but that particular example is worth examining carefully because string changes the answer depending on its ownership semantics.
Given Finch's rule, a returned loan must trace to a single input owner.  So if string is a non-Copy owned value and the default parameters are readonly borrows, then:

string which(string a, string b) {
    return len(a) > len(b) ? a : b;
}

cannot return a borrowed string, because statically the returned loan has two possible origins:
       &a ──┐
             ├──> result
       &b ──┘
Rust can encode that relationship:
fn which<'a>(a: &'a str, b: &'a str) -> &'a str

because 'a constrains both inputs to remain alive for the returned reference's lifetime.
Finch intentionally doesn't have the corresponding lifetime variable. The design document explicitly identifies essentially this exact class of function as the cost of the decision. 

But I wouldn't immediately conclude Finch needs lifetime parameters
There are actually two different questions here:
Can Finch express this zero-copy operation?

Currently, no—not with two borrowed inputs and a borrowed result.

Can Finch express the useful operation?

Certainly. For example, ownership can move through it:

string which(move string a, move string b) {
    return len(a) > len(b) ? a : b;
}

Semantically:
owned a ──┐
           ├── move winner ──> caller
owned b ──┘
              drop loser
That is perfectly safe and doesn't require lifetime reasoning at all.
But it changes the API significantly because the caller loses both arguments (with the unselected one destroyed). So it isn't an adequate substitute if the caller wants:

x = ...;
y = ...;

z = which(x, y);

use(x);
use(y);
use(z);

That's the real hole Gemini is identifying.
There's an interesting middle ground
I wouldn't necessarily jump from this to Rust-style named lifetime parameters.
The semantic fact Finch needs isn't really:

"a and b have lifetime 'x."
It's:
The result borrows from one member of a finite set of input owners.
Your existing contract currently supports:
returns-loan<arg(a)>
with exactly one origin. 
You could conceivably generalize that to something like:

returns-loan<arg(a), arg(b)>
meaning:
Any loan contained in the result originates from a or b, and therefore the caller must keep both possible owners valid while the returned loan exists.
Then:
string which(string a, string b)
    returnsLoan(a, b)
{
    return len(a) > len(b) ? a : b;
}

could work without introducing lifetime variables at all.
The caller's checker sees:

z = which(a, b)

z live
│
├── loan dependency → a
└── loan dependency → b
Therefore while z is live:
move a     ❌
mutate a   ❌ where conflicting
drop a     ❌

move b     ❌
mutate b   ❌ where conflicting
drop b     ❌
When z dies, both restrictions disappear.
That's conservative, but sound
At runtime z only points into one of them.
The compiler doesn't need to know which.

It simply treats the loan as depending on both.

Rust effectively accomplishes something closely related by unifying both arguments under 'a. But Finch could encode the relevant relationship directly in its existing place/loan system instead of introducing a general lifetime algebra.

And that seems much more consistent with Finch's philosophy.

You'd lose some expressiveness relative to Rust. For example, sufficiently complicated lifetime relationships may still be impossible. But:

returns-loan<arg(a), arg(b), ...>
would handle a surprisingly important family:
max_ref(a, b)
min_ref(a, b)
choose(condition, a, b)
find_in_either(a, b)
fallback(primary, secondary)

without exposing lifetime parameters.
It also generalizes nicely at control-flow joins
Internally Finch already has to join loan states across branches. 
So conceptually:

if (condition)
    return a;
else
    return b;

could produce:
branch 1 result origin = {a}
branch 2 result origin = {b}

join
result origin = {a,b}
That's a pretty natural abstract interpretation.
You could think of loan provenance as a small set:

Origin(result) ⊆ {parameters}
instead of the current:
|Origin(result)| = 1
The function interface publishes the resulting set.
This is dramatically smaller machinery than Rust regions.

And this is exactly the kind of abstraction hole your multiple-syntax strategy ought to expose.
It isn't fundamentally a C problem, Lisp problem, or Forth problem:

(define (which (a : string) (b : string)) : string
  (if (> (length a) (length b)) a b))
: which ( string string -- string )
    ... ;
string which(string a, string b) {
    return len(a) > len(b) ? a : b;
}

All three demand the same semantic concept:
a result loan whose provenance is a finite union of input owners.
So rather than adding frontend-specific tricks—or immediately importing Rust's lifetime system—the pressure from the examples is telling you that the shared ownership abstraction may be one notch too restrictive.
I think Gemini found a legitimate hole, but it may point toward multi-origin loan provenance, not toward “Finch needs Rust lifetimes.”
this doesn't seem like a huge problem, it just means you need copies some place.

e.g.

auto selected = which(a,b);
stealingFunc(b); // err
stealingFunc(Clone(B));
I agree. In fact, I think this example makes the current Finch rule look more reasonable, not less.
The important thing is what selected means. Under Finch's current rule, if which(a,b) could return a loan from either a or b, Finch can't represent that as a single-origin returned loan. But suppose Finch did support the conservative multi-origin loan we were discussing. Then:

auto selected = which(a, b);

stealingFunc(b);        // error: selected may borrow b
stealingFunc(Clone(b)); // fine: b remains alive

That's not especially onerous. It's arguably a good representation of what's happening.
The caller wants simultaneously to:

keep a reference that might point into b, and
transfer ownership of b somewhere else.
Those desires actually conflict. Somebody has to pay for creating an independent ownership path. A clone is an entirely reasonable place to make that cost visible.
And this fits Finch's broader principle extremely well: implicit operations may establish facts or create scoped views, but shouldn't silently allocate, retain, or move ownership. 

There's also an important distinction between expressiveness and ergonomics here.

Rust can express more precise lifetime relationships. That doesn't automatically mean Finch needs to. If Finch can say:

selected may depend on {a,b}

therefore:
    read a/b       ✓
    read selected  ✓
    move a/b       ✗ while selected lives
    mutate a/b     ✗ if it conflicts
    clone a/b      ✓
then the restriction is local, understandable, and has an obvious escape hatch.
And non-lexical last-use analysis makes this less annoying:

auto selected = which(a, b);

print(selected);   // last use of selected

stealingFunc(b);   // should now be fine

Finch already tracks loans and initialization per place and says reborrows/loans have bounded extents rather than necessarily lasting to lexical scope end.  So you wouldn't want selected unnecessarily locking both values until the closing brace.
I'm actually less convinced you need to reject which
My previous response accepted the existing “exactly one input owner” restriction too readily.
A finite provenance set seems like a small extension to the existing checker, not anything approaching Rust's lifetime machinery:

LoanOrigins(selected) = {a, b}
At the branch:
return condition ? a : b;

you just have:
origin(a) = {arg0}
origin(b) = {arg1}

join = union
     = {arg0,arg1}
The caller then conservatively maintains loans against every member until the returned loan dies.
No named lifetimes. No lifetime generics. No higher-ranked lifetimes. No variance over regions. No 'a syntax.

Just provenance sets.

The current spec deliberately restricts a returned loan to exactly one input owner.  That may be unnecessarily restrictive given how cheap the more conservative alternative is.

And there's a nice Finch-ish property here: when the conservative approximation becomes inconvenient, the programmer pays explicitly:

auto selected = which(a, b);

stealingFunc(Clone(b));

rather than making the type system increasingly sophisticated in order to prove that selected happened to refer to a.
That strikes me as very consistent with the language's philosophy: make the common safe case easy, conservatively reject conflicting ownership operations, and make the programmer explicitly pay when they need independent ownership.
stealingFunc wouldn't borrow. borrowing would be allowed right? it steals it. which is the error since b's lifetime ends at that point as it's moved?
Exactly. I misstated that distinction.
If selected may be borrowing from b, then borrowing b again is potentially fine, depending on the kind of borrow:

auto selected = which(a, b);

borrowingFunc(b);     // ✓ shared borrow alongside shared borrow
stealingFunc(b);      // ✗ moves b while selected may refer into b

The problem with stealingFunc(b) isn't that the function doesn't borrow. It's precisely that it takes ownership, so b is moved and the source binding becomes invalid. Finch explicitly says moves transfer the cleanup obligation and invalidate the source, while a live shared loan forbids mutation/move/drop of overlapping places.  
So:

auto selected = which(a, b);

stealingFunc(Clone(b));  // ✓
use(selected);           // ✓

works because you're moving the independently owned clone. The original b remains alive to satisfy selected's possible loan.
And with last-use analysis:

auto selected = which(a, b);

use(selected);       // loan ends here
stealingFunc(b);     // ✓ now

should also work.
The same reasoning distinguishes mutable borrowing:

auto selected = which(a, b);

readingFunc(b);      // ✓ shared borrow
mutatingFunc(b);     // ✗ exclusive borrow conflicts with possible shared loan
stealingFunc(b);     // ✗ move invalidates possible loan

So the rule is really beautifully simple:
As long as selected is live, every possible origin must remain valid and cannot undergo an operation incompatible with the returned loan.
That's why I'm increasingly persuaded that {a,b} provenance is sufficient for this case. You don't actually need to know which one selected points at. Conservatively borrowing both produces sensible behavior, and the programmer only encounters friction when they try to do something that could actually invalidate the reference.
That's considerably less problematic than “Finch can't handle functions like which.”