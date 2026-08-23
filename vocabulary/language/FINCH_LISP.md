# Finch Typed Lisp

Language version: `FINCH-LISP/1`

Finch Lisp is the default program-response syntax for models. It is eager, left-to-right, and
lexically scoped, and compiles directly to Finch typed stack IR without generating Forth text.
Use Co-Forth for an incrementally bufferable response or a short, obvious stack pipeline; use Lisp
for ordinary responses and for any program whose explicit nesting improves reliability.

`;` starts a line comment, and `#| ... |#` is a block comment. The executable-script shebang is
stripped by the host before this reader runs. Comments are non-semantic: they cannot grant a
capability, declare an effect, or change a signature.

The ordinary response is `(say "Hello")`. Pure and compound examples:

```lisp
(+ 3 (* 4 2))
(let ((a 10) (b 5)) (- a b))
(if (> 4 2) (say "yes") (say "no"))
(let ((n 10)) ((lambda ((x : int)) (+ x n)) 5))
(define (square (x : int)) (* x x))
(define (factorial (n : int)) : int
  (if (<= n 1) 1 (* n (factorial (- n 1)))))
(define (announce (n : int)) : unit ! (session.emit)
  (if (<= n 0) (say "done")
      (begin (say "tick") (announce (- n 1)))))
(define-syntax (when-positive test value) (if test value 0))
(when-positive (> 3 2) 42)
(define (singleton (x : int)) : list<int> (list x))
(list-get (list 4 8 15 16) 2)
(file-read (path "Cargo.toml"))
(file-write (path "generated/result.bin") data)
```

Lambda parameters use `(name : type)`. A lambda evaluates to a closure containing immutable code
plus explicit captured lexical values. Calling a closure is runtime behavior.

`define-syntax` provides bounded, capture-free template macros:
`(define-syntax (name parameter ...) template)`. A macro substitutes only its declared syntax
parameters, performs no evaluation, has no capabilities, and its expanded result is compiled and
verified normally. Expansion is capped at 128 forms per submission. To keep the version-1 system
hygienic by construction, a template may not introduce `let`, `lambda`, `define`, or
`define-syntax`; write a function for a new binding or accept the binding syntax from the caller.
There is no ellipsis, quasiquote, evaluator fallback, or compile-time I/O in this revision.

Core types include `unit`, `bool`, `int`, `uint`, `float`, `char`, `string`, `bytes`, and explicit
`dynamic` values. Parameterized spellings compose directly: `list<int>`, `map<string,int>`,
`option<T>`, `result<T,E>`, `task<T>`, `resource<kind>`, and `capability<kind>`.

`'name` (or `(quote name)`) produces a typed `symbol`, which is an identifier value rather than
text. `some`/`none` construct `option<T>` values; `ok`/`err` construct `result<T,E>` values;
`is-some`, `is-ok`, `unwrap`, `result-unwrap`, and `result-error` inspect or project them with
structured diagnostics on invalid branches.

`match-option` is a total, typed option branch. Its `some` arm binds the unwrapped value
lexically and its `none` arm receives no synthetic value; both arms must return the same type:

```lisp
(match-option (some 5)
  (some value (+ value 1))
  (none 0))
```

`match-result` is the corresponding total branch for `result<T,E>` values. It binds the selected
payload in each required arm:

```lisp
(match-result (ok 5)
  (ok value (begin value 0))
  (err problem (begin problem 0)))
```

`match` is the preferred type-directed spelling when the arm tags make the value kind clear. In
version 1 it accepts exactly the exhaustive `some`/`none` and `ok`/`err` pairs and lowers to the
same branch IR as the explicit forms; it does not perform dynamic dispatch:

```lisp
(match (some 5)
  (some value (+ value 1))
  (none 0))
```

MemTree and scheduling are explicit effects. Scheduled work stores an immutable program reference,
typed arguments, budgets, context references, and a revocable policy reference—not raw authority or
an unvalidated Lisp string. A callback starts a fresh audited task and revalidates its environment
and grants when it fires.

Loops use `(while condition body...)`. The condition must be `bool`, the body must preserve the
surrounding stack shape, and every back-edge consumes execution fuel and observes cancellation.
Capabilities in the condition or body are inferred even if a branch does not execute at runtime.

Use `(while :label name condition body...)` for a loop that needs a structured named exit. Inside
its body, `(break name)` exits to that loop's typed exit edge and `(continue name)` returns to its
condition edge. The named exit must preserve the target loop's stack row and currently carries no
extra result values; it is not an arbitrary jump.

The version-1 typed frontend currently accepts literals, homogeneous non-empty lists, core
vocabulary calls, `begin`, lexical `let`, typed `if`, typed `while`, typed `lambda`, closure calls,
and typed function definitions using `(define (name (arg : type) ...) body...)`. For self- or
mutually-recursive functions, put a return type after the header:
`(define (name (arg : type) ...) : result-type body...)`. Finch predeclares that signature before
compiling bodies, so recursive calls remain type-checked. A return-annotated definition defaults to
the pure effect bound. An effectful recursive definition must declare the upper bound explicitly,
for example `(define (announce (n : int)) : unit ! (session.emit) ...)`. In version 1 the list
contains only named, unscoped capability identities such as `session.emit`, `memory.read`, or
`schedule.create`; it is a bound on inferred effects, not an authority grant. Parameterized
resource selectors remain inferred from typed calls and require a later selector-annotation syntax.
Definitions enter the persistent shared dictionary only if the entire submission verifies, is
authorized, and commits.

The first string immediately after a `define` header, optional return annotation, and optional
effect annotation is a docstring,
following Common Lisp/Python practice. It is retained in the immutable definition metadata and
in the typed function contract, but omitted from executable instructions, so it neither pushes a
runtime value nor changes the function's cost. VM inspection and vocabulary discovery can return
it without reparsing source:

```lisp
(define (factorial (n : int)) : int
  "Return n factorial for non-negative n."
  (if (<= n 1) 1 (* n (factorial (- n 1)))))
```

Co-Forth uses the corresponding leading `\\ finch-doc:` spelling; see the Co-Forth language
definition. Comments remain non-executable metadata; they never grant authority or change a word
signature.

Legacy variable definitions and other unsupported legacy forms are not part of the provider
language. They require an explicitly named host migration API; ordinary program submission never
falls back to a compatibility evaluator.

Use only functions in the current VM manifest. Call `get_vm_state` and vocabulary introspection
instead of guessing names or signatures. Submit raw source through `submit_program` with the observed
manifest generation and expected VM revision.

`path` creates a workspace-relative refined path. `file-read` produces `bytes`; `file-write`
consumes a refined path and bytes. These are capability-bearing host calls and may require an
approval before execution.

`host-path` creates the distinct `path<host-machine>` type. It is usable only with
`host-file-read` / `host-file-write`, after the host explicitly installs `root<host-machine>` and
the user grants the concrete file selector. It is not an absolute-string escape hatch; ordinary
workspace file words cannot consume it.

For large text, CSV, or binary files, use `(file-size path)` and `(file-slice path offset length)`
instead of `(file-read path)`. A slice returns at most the requested range (currently capped at
8 MiB per call), with a shorter final value at EOF. Keep the byte offset in a lexical binding and
process each slice before requesting the next. For UTF-8 line-oriented files,
`(file-lines-open path)` returns a host-issued `resource<file-line-cursor>` and
`(file-lines-next cursor)` returns `option<string>` one bounded line at a time (1 MiB maximum
line); `(file-lines-close cursor)` releases it early. The cursor is owned by its ProgramRun and
cannot be forged or reused by another run. CSV-record cursors are distinct because quoted fields
may span physical lines; workbook cursors remain future resources. An Excel document must not be
treated as a single text string.

```lisp
(let ((cursor (file-lines-open (path "data.csv"))))
  (file-lines-next cursor))
```

To process a text file without retaining prior rows, put the cursor in a named loop and make the
`none` arm take the typed exit. The final close runs after the loop; production code should also
close a cursor on any explicit early-return/error path until structured cleanup is added:

```lisp
(let ((cursor (file-lines-open (path "data.csv"))))
  (begin
    (while :label rows true
      (match-option (file-lines-next cursor)
        (some line (say line)) ; replace with bounded row processing
        (none (break rows))))
    (file-lines-close cursor)))
```

CSV must use a record cursor rather than a line cursor because quoted fields can contain commas and
newlines. `(csv-open path)` returns `resource<csv-record-cursor>`;
`(csv-next cursor)` returns `option<list<string>>`; `(csv-close cursor)` releases it. Fields are
UTF-8, quoted fields follow RFC-style doubled-quote and multiline rules, malformed quote boundaries
are rejected, and each complete record is limited to 8 MiB.

```lisp
(let ((cursor (csv-open (path "data.csv"))))
  (begin
    (match-option (csv-next cursor)
      (some record (say (list-get record 0)))
      (none (say "No records.")))
    (csv-close cursor)))
```

`(output-open "title")` returns an opaque `resource<output-handle>` issued by the host. Use that
explicit handle with `(output-append handle text)`, `(output-replace handle text)`,
`(output-status handle text)`, `(output-progress handle completed total)`,
`(output-complete handle)`, or `(output-fail handle text)`. These are portable side-effect events,
not direct terminal mutation or an implicit global WorkUnit.

The handle is owned by its ProgramRun, survives that run's verified suspension/resumption, and is
rejected if a different submission tries to reuse it.

Agent coordination uses persistent typed `task<string>` handles: `(agent-spawn "task")`,
`(agent-poll task)`, `(agent-await task)`, and `(agent-cancel task)`. A task value may remain in
the VM across later turns; poll while work is running and await only when the final result is
needed.

`(defer :cpu (lambda () expression))` starts a pure zero-argument closure on a bounded private CPU
worker and returns `task<T>`. Captures are immutable snapshots, not references to the parent stack.
Within Lisp, bind that handle normally and use `(task-poll handle)` for `option<T>` or
`(task-join handle)` for `T`; a running join suspends the VM continuation rather than blocking the
event loop. `(task-cancel handle)` consumes a `cpu_fiber` handle and requests cooperative
cancellation; a worker observes that request at its next VM boundary. CPU task operations reject
agent-task handles; use `agent-cancel` for child agents.

`(yield)` is a cooperative statement expression. Finch records the remaining typed VM frames,
returns control to its event loop, and later resumes the following expression. It evaluates to
`unit`; it is not a first-class continuation and does not terminate the program:

```lisp
(begin
  (say "Searching…")
  (yield)
  (say "Finished."))
```

`yield` is not a sequence generator: version 1 has no `next`, lazy `generator<T>`, or iterable
protocol. Do not infer those features from the legacy Co-Forth compatibility library. Use a bounded
list for a known finite result, a cursor (`file-lines-next`/`csv-next`) for host-backed iteration,
or ask for/add a typed stream primitive when lazy generated sequences are required.

`(vm-vocabulary)` returns the current typed word manifest for programmatic introspection.

`(process-run command (list arguments...))` runs an executable directly without shell parsing and
is capability-bearing.
