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
(list-get (list-append (list 4 8) 15) 2)
(empty-list string)
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

When a definition returns one compatible `result<R,E>`, `(try expression)` short-circuits an
`err(E)` back to that definition's caller and otherwise evaluates to the `ok` payload. It is not
an exception and does not make host effects retryable:

```lisp
(define (parse-config (text : string)) : result<json,string>
  (ok (try (json-parse text))))
```

`match` is the preferred type-directed spelling when the arm patterns make the value kind clear.
It accepts exhaustive `some`/`none` and `ok`/`err` pairs and lowers to the same branch IR as the
explicit forms. It also accepts a total boolean pair and a finite integer switch with a required
final `_` arm. These are statically typed branch forms, not dynamic dispatch:

```lisp
(match (some 5)
  (some value (+ value 1))
  (none 0))

(match true
  (true 42)
  (false 0))

(match 2
  (0 100)
  (2 42)
  (_ 0))
```

Integer literal arms must be unique, and `_` must be the final arm. A statically typed record has
one total destructuring arm whose `(field binding)` pairs may select any known subset; `_` validates
a field without binding it:

```lisp
(match { :name "Ada" :age 37 }
  (record ((name who) (age years))
    (+ years 5))) ; => 42
```

This is syntax over the same `record-get` plus typed-local operations available in Co-Forth; it is
not a Lisp-only runtime matcher. A typed list has an exhaustive `empty`/`cons` pair. The `cons` arm
binds its head and immutable tail:

```lisp
(match (list 4 8)
  (empty 0)
  (cons head tail (+ head (list-length tail)))) ; => 5
```

This lowers through `list-uncons`, option branching, record projection, and locals. Strings and
arbitrary JSON have no general pattern matcher yet: use their explicit typed operations rather than
falling back to a dynamic dispatcher.

`list-uncons` is the safe shared decomposition primitive beneath that syntax. It returns `none` for
an empty list or `some(record{head:A,tail:list<A>})`, avoiding an exception or a language-special
multi-return:

```lisp
(match-option (list-uncons (list 4 8))
  (some pair (unwrap (record-get pair "head")))
  (none 0)) ; => 4
```

`{ ... }` constructs a typed immutable product from `:name value` field forms. Each field keeps its own
type; it is not an untyped JSON object. `record-get` takes a literal field name and returns an
`option<T>`, so a record can cross a general boundary without a crash-producing implicit field
access. Dynamic object traversal remains the explicit `json-*` boundary:

```lisp
(unwrap (record-get { :name "Ada" :age 37 } "age")) ; => 37
(unwrap (record-get (record-set { :name "Ada" :age 37 } "age" 38) "age")) ; => 38
```

`record-set` creates a new record rather than mutating the original. Its field name is also a
literal string, and the replacement must match the field's statically known type.

Closed variants use an explicit full type and selected tag. A payload is present exactly when the
declared tag requires one:

```lisp
(variant variant{none|some(int)} :none)
(variant variant{none|some(int)} :some 42)
```

The full type prevents an isolated tag value from losing the other alternatives needed for
exhaustive checking.
`(variant-get value :some)` safely returns `option<int>` for the example type. A payload-free tag
returns `option<unit>`, allowing the same operation to support ordinary exhaustive branching.
Closed variants support exhaustive patterns. Payload tags bind one name; payload-free tags do not:

```lisp
(match value
  (none 0)
  (some number (+ number 1)))
```

Every declared tag must appear exactly once and every arm must produce the same type. Use the
explicit `match-variant` spelling when tag names intentionally collide with built-in option/result
patterns such as `some` followed by `none`.

Records may contain ordinary typed closure values, which makes an immutable object-style value
possible without a second object runtime. Method invocation remains explicit: project, unwrap, and
call the closure. A later method-sugar layer may pass `self` explicitly, but no closure gains
ambient mutable access to the record that contains it.

```lisp
(let ((object { :run (lambda ((x : int)) (+ x 1)) }))
  ((unwrap (record-get object "run")) 41)) ; => 42
```

`map-entries` converts a typed map into an insertion-ordered `list` of typed
`{ :key ... :value ... }` records. This is the portable basis for inspecting or looping over both
keys and values without dropping into untyped JSON:

```lisp
(unwrap (record-get
  (list-get (map-entries (map "answer" 42)) 0)
  "value")) ; => 42
```

External JSON remains `json`, rather than being silently coerced into a typed record. Its keys may
contain spaces or other arbitrary text; access them with a string through `json-get`, or normalize
the input into a string-keyed typed map when its schema is known:

```lisp
(unwrap (json-as-string
  (unwrap (json-get
    (result-unwrap (json-parse "{\"first name\":\"Ada\"}"))
    "first name")))) ; => "Ada"
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

Type annotations use the same compact grammar as Co-Forth signatures. In addition to
`list<T>`, `map<K,V>`, and `result<T,E>`, `fn<A,B,R>` describes a pure closure taking `A,B` and
returning `R`; `fn<R>` describes a pure zero-argument closure. A fixed product type is
`record{name:string,age:int}`. It describes a known field set and is distinct from an open map;
record values are constructed with `{ :name value :age value }` and projected with `record-get`.

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

A top-level `begin` may group definitions with executable forms in one response. Finch splices that
container before predeclaring definitions, so recursive definitions are immediately available to
later siblings while `define` inside a function, branch, or lexical expression remains invalid:

```lisp
(begin
  (define (factorial (n : int)) : int
    (if (<= n 1) 1 (* n (factorial (- n 1)))))
  (say (int-to-string (factorial 6))))
```

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

`project-path` and `task-output-path` similarly produce distinct refined types for optional
application-installed roots. Use `project-file-read` / `project-file-write` and
`task-output-file-read` / `task-output-file-write` respectively. This permits narrow policies such
as project-read plus task-output-write, and the verifier rejects mixing paths between roots.

For large text, CSV, or binary files, use `(file-size path)` and `(file-slice path offset length)`
instead of `(file-read path)`. A slice returns at most the requested range (currently capped at
8 MiB per call), with a shorter final value at EOF. Keep the byte offset in a lexical binding and
process each slice before requesting the next. Use `(tree-list directory max-entries)` for bounded,
deterministic directory discovery. It returns
`{entries:list<record{path:string,kind:string,size:int}>,truncated:bool}`; returned path strings
carry no authority until refined with `path`. For UTF-8 line-oriented files,
`(file-lines-open path)` returns a host-issued `stream<string>` and `(stream-next stream)`
returns `option<string>` one bounded line at a time (1 MiB maximum line); `(stream-close stream)`
releases it early. The stream is owned by its ProgramRun and cannot be forged or reused by another
run. `file-lines-next` / `file-lines-close` remain compatibility aliases. CSV record streams are
distinct because quoted fields
may span physical lines. `(workbook-open path)` opens the first XLSX/XLS/ODS sheet and
`(workbook-sheet-open path sheet)` opens a named sheet as `stream<list<string>>`; use ordinary
`stream-next` and `stream-close`. The host currently caps a decoded sheet at 10 million cells while
passing only one row into the VM at a time; it is not yet a true streaming ZIP/XML decoder.

```lisp
(let ((stream (file-lines-open (path "data.csv"))))
  (stream-next stream))
```

To process a text file without retaining prior rows, put the cursor in a named loop and make the
`none` arm take the typed exit. The final close runs after the loop; production code should also
close a cursor on any explicit early-return/error path until structured cleanup is added:

```lisp
(let ((stream (file-lines-open (path "data.csv"))))
  (begin
    (while :label rows true
      (match-option (stream-next stream)
        (some line (say line)) ; replace with bounded row processing
        (none (break rows))))
    (stream-close stream)))
```

CSV must use a record stream rather than a line stream because quoted fields can contain commas and
newlines. `(csv-open path)` returns `stream<list<string>>`;
`(stream-next stream)` returns `option<list<string>>`; `(stream-close stream)` releases it. Fields are
UTF-8, quoted fields follow RFC-style doubled-quote and multiline rules, malformed quote boundaries
are rejected, and each complete record is limited to 8 MiB.

For bounded schema discovery and column statistics, prefer `(csv-summary path max-rows)` over
materializing records. It treats the first record as headers, scans at most 100,000 data records,
and returns managed `json` containing `headers`, `sampled_rows`, `truncated`, and per-column `empty`,
`non_empty`, `numeric`, `min`, `max`, and `mean` fields. A row wider than its header is rejected.

Use `(workbook-sheets path)` to discover sheet names. `(workbook-range path sheet start-row
start-column row-count column-count)` returns a zero-based rectangular slice of at most 10,000
cells as `list<list<string>>`. `(workbook-summary path sheet max-rows)` treats the first sheet row
as headers and returns the same bounded per-column facts as `csv-summary` plus the sheet name.

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

`output-append` and `output-replace` affect the handle body. `output-status` is separately
rendered transient status text, and `output-progress` is independent progress metadata; neither
operation erases body text. All handle updates are effect-only expressions and leave no public
value on the shared stack.

The handle is owned by its ProgramRun, survives that run's verified suspension/resumption, and is
rejected if a different submission tries to reuse it.

Agent coordination uses persistent typed `task<agent-result>` handles: `(agent-spawn "task")`,
`(agent-poll task)`, `(agent-await task)`, and `(agent-cancel task)`. A task value may remain in
the VM across later turns; poll while work is running and await only when the final result is
needed. Poll returns a typed snapshot record with status, identity, task/role/model, depth, and a
`complete` flag. Await returns a typed result record with status, identity, final message,
diagnostics, turn/timing metadata, model, and depth. Use `record-get` to inspect those fields; no
JSON or message parsing is required. Use `agent-spawn-with` when the child needs an explicit role,
bounded parent-authored background, provider/model selection, or tighter budgets:

```lisp
(agent-spawn-with {
  :task "inspect recent failures"
  :role "explore"
  :background "focus on typed effects"
  :provider ""
  :model ""
  :context-refs (list {
    :kind "artifact"
    :id "failure-log"
    :sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" })
  :capabilities (empty-list resource<capability-grant>)
  :max-turns 4
  :timeout-ms 60000
  :max-output-bytes 65536 })
```

Roles are `general`, `explore`, `research`, or `code`. Empty background/provider/model strings
mean no override. Use `(empty-list record{kind:string,id:string,sha256:string})` when there are no
context references. References and the effective `starting-context-hash` make child inputs
auditable. The host registers immutable bounded UTF-8 artifacts and supplies their computed
references; child creation resolves and SHA-256 verifies the bytes before allocating a task. An
unknown, rebound, oversized, non-text, or mismatched artifact fails closed. An unavailable explicit
provider/model likewise fails before a child task is created. Use `(capability-list)`
to discover live delegable grants, extract a chosen entry's opaque `grant` resource with
`record-get`, and place those resources in `:capabilities`. An empty capability list delegates no
non-intrinsic authority; UUID strings and the adjacent JSON requirement metadata cannot be used in
their place. The compact `(agent-spawn task)` form deliberately inherits the caller's entire
creation-time ceiling.

`(defer :cpu (lambda () expression))` starts a pure zero-argument closure on a bounded private CPU
worker and returns `task<T>`. Captures are immutable snapshots, not references to the parent stack.
Within Lisp, bind that handle normally and use `(task-poll handle)` for
`record{task:task<T>,value:option<T>}` or `(task-join handle)` for `T`. The poll record preserves the
handle explicitly for a later poll/join/cancel; a running join suspends the VM continuation rather than blocking the
event loop. `(task-cancel handle)` consumes a `cpu_fiber` handle and requests cooperative
cancellation; a worker observes that request at its next VM boundary. CPU task operations reject
agent-task handles; use `agent-cancel` for child agents.

`(yield value)` publishes one typed value, records the remaining typed VM frames, and returns
control to its event loop as a saved suspension. `(yield)` is exact shorthand for `(yield nil)`,
the cooperative-timeslice case. A host must later resume that same execution. It evaluates to
`unit`; it is not a first-class continuation and does not terminate the program:

```lisp
(begin
  (say "Searching…")
  (yield)
  (say "Finished."))
```

Use `(defer closure)` (or `(defer :fiber closure)`) to turn a pure zero-argument yielding closure
into `fiber<Y,R>`. `(fiber-next fiber)` returns `ok(Y)` for the next yielded value, or
`err(end(R))` after the closure returns. `(fiber-join fiber)` discards remaining yields and returns
`R`; `(fiber-cancel fiber)` makes later use fail deterministically. Bind or otherwise retain the
handle when advancing it because these operations consume their argument like ordinary functions:

```lisp
(let ((numbers (defer (lambda () (begin (yield 2) (yield 3) 5)))))
  (list (fiber-next numbers) (fiber-next numbers) (fiber-next numbers)))
```

Producer continuations are checkpointable and ProgramRun-transactional. This version resumes them
with `unit` and permits only pure closures. Use a bounded list for a known finite result or a typed
`stream<T>` with `stream-next` / `stream-close` for host-backed iteration.

`(vm-vocabulary)` returns the current typed word manifest for programmatic introspection.

JSON values have an explicit typed boundary. `(json-parse text)` returns `result<json,string>`;
`(json-get value field)` returns `option<json>` only for an object field, and `(json-as-string ...)`,
`(json-as-int ...)`, `(json-as-float ...)`, and `(json-as-bool ...)` return an option instead of silently coercing. For
example:

```lisp
(unwrap
  (json-as-int
    (unwrap
      (json-get
        (result-unwrap (json-parse "{\"answer\":42}"))
        "answer"))))
```

Use `(json-stringify value)` only when text serialization is actually needed. User text inside JSON
never becomes a capability, path, or executable form merely by being parsed.
`(json-index value index)` returns `option<json>` for array data, while `(json-keys value)` returns
`list<string>` for an object and an empty list for any other JSON value. `(json-as-map value)`
explicitly normalizes an object to `option<map<string,json>>`, preserving arbitrary string keys;
it returns `none` for non-objects and never guesses a typed-record schema.

The generic MCP boundary is `(mcp-call server tool arguments-json)`. It returns managed `json` and
requires authority for that exact server/tool pair. Construct arguments through `json-parse` in
Lisp source. Discovered tools appear as `mcp.<server>.<tool>` and consume one schema-derived typed
record when possible, otherwise one managed `json`. For example:

```lisp
(mcp.github.issue_get { :owner "darwin-finch" :repo "finch" :issue_number 42 })
```

Inspect the binding first: its immutable schema hash versions the discovered contract and a refresh
increments the VM manifest generation.

Typed maps are immutable shared-language collections, separate from JSON. Construct one with
alternating key/value forms: `(map "answer" 42 "other" 7)`. All keys must share one type and all
values must share one type. `(map-get table key)` returns `option<V>`; `(map-set table key value)`
returns a replacement map; `(map-keys table)` returns `list<K>`; and `(map-length table)` returns
an integer. Start an incrementally built map with `(empty-map key-type value-type)`. Later duplicate
keys replace the earlier value:

```lisp
(unwrap (map-get (map-set (map "answer" 42) "answer" 99) "answer"))
```

`(process-run command (list arguments...))` runs an executable directly without shell parsing and
is capability-bearing.
