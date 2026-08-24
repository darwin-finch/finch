# Finch Typed Co-Forth

Language version: `FINCH-FORTH/1`

Co-Forth is the user/model-facing textual form of Finch's typed stack IR. It is deliberately close
to one-to-one: a literal becomes `Constant`, a word call becomes `Call`, `if`/loops become explicit
IR control-flow blocks, and local syntax becomes `LocalSet`/`LocalGet`. The binary/serialized IR
remains the internal representation, but there is no hidden second execution semantics. Source is
whitespace-separated and evaluated left-to-right against the recipient's typed data stack.

```forth
3 4 2 * +
s" Hello from Finch" say
```

Booleans are `true` and `false`. The stack manifest is ordered bottom-to-top. Never assume it is
empty; inspect it and include `expected_revision` when manipulating existing values.

`say` and every `output-*` operation are stack-neutral effects: they consume their arguments and
leave no `unit` placeholder. Compose consecutive output directly—never add `drop` after them.
Lisp keeps `unit` only as an internal expression value; at a program boundary it is likewise not
persisted onto the shared stack.

`cr` is the conventional explicit line break, exactly equivalent to `s"\n" say`. It does not
change the exact-chunk behavior of `say`.

`\` starts a line comment outside a string and continues through the following newline. Comments
are ignored by the compiler and never grant capability, change a signature, or affect provenance.

`'name` pushes a typed symbol value. It is data, not a dictionary lookup; `['] word execute`
remains the form for obtaining and invoking a typed word reference. `some`/`none` and `ok`/`err`
construct typed option/result values; use `is-some`, `is-ok`, `unwrap`, `result-unwrap`, and
`result-error` for inspection and projection.

`if-some` is the structured option branch. It consumes an `option<T>`, makes the unwrapped `T`
available on the then stack, and consumes the `none` value on the required `else` stack. Both
branches must leave the same typed stack row:

```forth
5 some if-some
  1 +
else
  0
then
```

`if-ok` is the equivalent structured result branch. It consumes a `result<T,E>`, supplies `T` to
the then branch and `E` to the required `else` branch:

```forth
5 ok if-ok
  drop
else
  drop
then
```

`{ ... }` constructs an immutable heterogeneous record. `name:` is a syntax
label (not a stack value), so every label must have exactly one following value. Project a known
field with `"name" record-get`; it produces `option<T>` and can be checked with `unwrap` or the
typed option branch forms:

```forth
{ name: "Ada" age: 37 } "age" record-get unwrap  \ leaves 37
{ name: "Ada" age: 37 } 38 "age" record-set "age" record-get unwrap  \ leaves 38
```

`record-set` creates a new record; it does not mutate the original value. Its final field name is
a literal string, and the replacement must have the field's statically known type.

`record-get:<name>` remains accepted as compatibility syntax, but new programs should use the
ordinary stack spelling `"name" record-get`.

Records may also hold typed quotations. This lets a record carry data and a reusable operation
without introducing a separate object runtime; invocation remains explicit and receives its inputs
on the ordinary stack:

```forth
: increment ( S int -- S int ! pure ) 1 + ;
{ run: ['] increment } "run" record-get unwrap 41 swap execute  \ leaves 42
```

`map-entries` returns an insertion-ordered `list<record{key:K,value:V}>`, which can be traversed
using the ordinary list and loop forms:

```forth
map{ "answer" 42 }map map-entries 0 list-get "value" record-get unwrap  \ leaves 42
```

External JSON remains `json`, rather than being silently coerced into a typed record. Its keys may
contain spaces or other arbitrary text; access them with a string through `json-get`:

```forth
s"{\"first name\":\"Ada\"}" json-parse result-unwrap
s"first name" json-get unwrap json-as-string unwrap  \ leaves "Ada"
```

Co-Forth accepts a pasted JSON object directly when its first key is quoted, so commas and ordinary
JSON string escaping work without translating it into a record or map literal. The value remains
managed `json`; `{ name: "Ada" }` with an identifier label is still a typed record. Commas are
also optional separators in typed list source, including tight pasted forms:

```forth
{"first name":"Ada","age":37} "first name" json-get unwrap json-as-string unwrap
[1, 2, 3] 2 list-get                 \ leaves 3
```

`case` is a typed integer switch with no fallthrough. Each `of ... endof` arm and the optional
`otherwise` arm must leave the same stack row. The selector is removed before an arm begins;
`otherwise` is required whenever an unmatched case must produce values. This is structured branch
syntax, not a dynamic dictionary lookup:

```forth
2 case
  1 of 10 endof
  2 of 20 endof
  otherwise 30
endcase                 \ leaves 20
```

Without `otherwise`, a non-matching selector is simply dropped, so every selected arm must also
leave no values. Use a typed `if`/`if-some`/`if-ok` when the branch condition is not an integer
selector.

Typed signatures must use `S` as the first item on both sides of `--`. It is the preserved unknown
lower caller stack, so ordinary words consume only their declared inputs and preserve everything
beneath them:

```forth
: square ( S int -- S int ! pure )
  dup *
;
```

`S` is a type-level stack row, not a runtime value: `( S -- S ! pure )` is stack-neutral, not a
word that consumes the whole stack. A typed definition may not omit, duplicate, or drop this row.

Type names may be parameterized in a signature, including nested forms such as `list<int>`,
`map<string,int>`, `option<list<string>>`, `result<T,E>`, `task<T>`, `resource<kind>`, and
`capability<kind>`. A fixed product type is `record{name:string,age:int}`; it is distinct from
an open `map<string,T>` and may be the input or output of a typed word.

Name inputs directly in the typed signature when the body needs them. Names are in bottom-to-top
stack order and lower directly to frame locals; there is no separate locals declaration:

```forth
: area ( S width:int height:int -- S int ! pure )
  width height *
;
```

This lowers to `LocalSet height`, `LocalSet width`, then `LocalGet width` and `LocalGet height`.
The frame owns those locals and a private operand window above the caller stack boundary. `return`
keeps only the declared output values from that window, then destroys the frame; temporary values
cannot leak into the caller.

Use `! pure` to assert purity or `! infer` to accept the transitively inferred capability set in
the current frontend. A false purity
assertion rejects the entire submission and does not modify the dictionary. Declared-pure definitions are predeclared as a group, so they may be self- or
mutually-recursive in one submission; `! infer` definitions remain sequential until their inferred
effects can be represented in a forward signature. Typed definitions are persistent and immediately
callable from Finch Lisp.

Put a public doc comment immediately before a typed definition when it should be discoverable from
the shared VM without reparsing source:

```forth
\ finch-doc: Double an integer.
: double ( S int -- S int ! pure ) 2 * ;
```

Only the text after `finch-doc:` is retained on the immutable typed function contract. It is never
placed on the operand stack or executed, and inspection may return it alongside the signature.

`dup`, `drop`, and `swap` are polymorphic. Arithmetic does not coerce strings or dynamic values.
Control-flow merge points must have identical stack types, and loops require stable invariants.
Checked arithmetic reports division-by-zero or overflow traps.

Workspace file words use refined paths: `s" Cargo.toml" path file-read` leaves `bytes`, while
`s" generated/result.bin" path data file-write` consumes the bytes and leaves no stack value.
Paths are
workspace-relative and checked against their declared selector at both verification and host
execution. Missing file grants pause for approval rather than being silently widened.

For large text, CSV, or binary files, prefer `file-size` and bounded `file-slice` instead of
materializing the whole file. `path offset length file-slice` reads at most the requested byte
range (currently capped at 8 MiB per call) and returns `bytes`; EOF simply returns a shorter final
slice. Maintain an explicit byte offset in a local and decode/process each slice before requesting
the next.

`path file-hash` computes a lowercase SHA-256 digest without putting the file's bytes on the VM
stack. Use it for exact comparison or as a leaf value in a higher-level tree/Merkle operation.
`path tree-merkle` computes a deterministic SHA-256 digest over a directory subtree's sorted
relative paths and file hashes, without materializing its contents. It is bounded to 100,000
entries and rejects symlinks rather than following them. Use it as an inventory/change-detection
fact; it is not by itself a malware verdict or permission to alter the tree.

`host-path` is a separate, host-issued root type, never an absolute-path bypass. If the host has
explicitly installed `root<host-machine>` and the user has approved a matching selector, use
`s" var/log/system.log" host-path host-file-read`. Workspace `file-read` and `file-write` reject
that value at verification time; bind `/` only for an intentional whole-machine grant.

For UTF-8 line-oriented files, `file-lines-open` returns a host-issued `stream<string>` and
`stream-next` returns `option<string>` one bounded line at a time (1 MiB maximum line);
`stream-close` releases it early. The stream is owned by its ProgramRun and cannot be forged or
reused by another run. `file-lines-next` / `file-lines-close` remain compatibility aliases.
CSV-record streams are distinct because
quoted fields may span physical lines; workbook cursors remain future resources. Do not fake
spreadsheet streaming by loading a whole workbook into a string.

```forth
: first-line ( S -- S option<string> ! infer )
  s" data.csv" path file-lines-open
  stream-next
;
```

For a bounded streaming pass, keep the cursor as the loop row and use `if-some` to consume each
record without retaining it between iterations:

```forth
s" data.csv" path file-lines-open
begin: rows true while
  dup stream-next if-some
    say                      \ replace with bounded row processing
  else
    break rows
  then
repeat
stream-close
```

Use `csv-open` to obtain `stream<list<string>>`, `stream-next` to obtain
`option<list<string>>`, and `stream-close` to release it. The stream accepts UTF-8 RFC-style quoted
fields, including doubled quotes and multiline fields, and bounds each complete record to 8 MiB.
It rejects malformed quote boundaries instead of guessing. It has the same ProgramRun ownership and
`file.read` requirement as a line cursor.

```forth
s" data.csv" path csv-open
dup csv-next if-some
  0 list-get say       \ first field of one CSV record
else
  s" No records." say
then
drop                    \ branch result; retain the cursor
csv-close
```

Agent words operate on persistent typed `task<string>` handles: `s" task" agent-spawn`,
`agent-poll`, `agent-await`, and `agent-cancel`. A handle can remain on the VM stack across later
turns. Poll is nonblocking and returns serialized status; await returns the final message.

For CPU-bound pure closures, `['] zero-argument-word defer-cpu` starts a private worker and leaves
`task<T>`. `task-poll` replaces it with `option<T>`; `task-join` replaces it with `T`, suspending
the current VM run if necessary rather than blocking the terminal. `task-cancel` consumes a handle
and requests cooperative cancellation at its next worker VM boundary. CPU task words reject
agent-task handles.

`yield` is a stack-neutral cooperative scheduling point. It yields control to Finch's event loop
as a saved suspension; a host must explicitly resume that execution at the next word. Finch does
not yet automatically requeue yielded runs. It does not expose a continuation value or terminate
the program. Use it between bounded phases when another host-owned event should run:

```forth
s" Searching…" say
yield
s" Finished." say
```

`vm-vocabulary` returns the current serialized typed word manifest.

JSON is a managed typed value, not a stringly authority channel. Parse untrusted text with
`json-parse` (which returns `result<json,string>`), look up an object field with `json-get`
(which returns `option<json>`), then project a scalar through an option-returning converter:

```forth
s" {\"answer\":42}" json-parse result-unwrap
s" answer" json-get unwrap json-as-int unwrap
```

`json-as-string`, `json-as-int`, `json-as-float`, and `json-as-bool` return `none` for a mismatched JSON kind;
`json-index` returns `option<json>` for an array index, `json-keys` returns `list<string>` for an
object (or an empty list otherwise), `json-as-map` explicitly normalizes an object to
`option<map<string,json>>`, and `json-stringify` converts a managed JSON value back to compact
text. `json-as-map` preserves arbitrary keys such as `"first name"`; it does not coerce JSON into
a typed record.

`process-run` consumes a command string and a list of argument strings; it never invokes a shell.

For progressive prose, emit multiple typed chunks with `say`. Bare `"..."` pushes one typed
`string` and never evaluates its contents. `s"..."` is an equivalent familiar Forth spelling; the
`s` means **string**, not `say`. Both `s"text"` and the conventional `s" text"` spell the
identical string `text`: exactly one ASCII whitespace delimiter immediately after `s"` is ignored.

Standard Forth `."..."` is supported as output shorthand: it lowers exactly to `s"..." say`,
including the normal typed `session.emit` side effect. Use `s"..."` when a string must be passed
to another word; use either `say` or `."..."` when it is user-visible output.
Use escapes (`\"`, `\\`, `\n`, `\r`, `\t`) inside this short-string form:

```forth
"Hello user" say
s" Hello user" say  \ same string: "Hello user"
." Hello user"       \ output shorthand; same as s" Hello user" say
```

For prose or copied text containing quotes/newlines, use a raw triple-quoted literal. Its contents
are verbatim until the next `"""`; it has no escapes and preserves all leading whitespace:

```forth
"""The user said "hello".
Second line.""" say
```

`s"""..."""` remains a compatible spelling.

The delimiter `"""` itself cannot appear in a raw literal; split the text into ordinary typed
string operations if it is needed.

Typed maps are immutable shared-language collections. Co-Forth uses `map{` / `}map` around an
even sequence of key/value expressions; the closing delimiter lowers the suffix directly to typed
IR. `map-get` returns `option<V>`, `map-set` returns a replacement map, `map-keys` returns
`list<K>`, and `map-length` returns an integer. Use `empty-map<K,V>` for an explicitly typed empty
map. Later duplicate keys replace earlier values:

```forth
map{ s" answer" 42 s" other" 7 }map
s" answer" map-get unwrap
```

Typed lists are likewise immutable. Use `[` / `]` around one or more homogeneous values;
`list-append` returns a replacement list rather than changing the original. `['] word` remains a
distinct quotation form. An empty list has no inferable element type, so use `empty-list<T>` to
state one explicitly. `list{` / `}list` remains a compatibility spelling while existing scripts
migrate:

```forth
[ 1 2 ] 3 list-append
2 list-get                 \ leaves 3
empty-list<string>
```

`say` is a stack-neutral response effect: it consumes its string and leaves no value on the
stack. Consecutive `say` calls compose directly; do not add `drop` after one:

```forth
s" Working…" say
2 3 + int-to-string say
```

Lisp keeps an internal `unit` only while compiling an effect-only expression; at a program
boundary it likewise leaves no synthetic value on the shared stack. Both forms emit the same
ordered response chunks.

For concurrent/reactive presentation, `output-open` asks the host for an opaque
`resource<output-handle>`. Keep that handle explicitly (usually in a local), then use
`output-append`, `output-replace`, `output-status`, `output-progress handle completed total`,
`output-complete`, or `output-fail`. These produce portable ordered UI events; they do not mutate
the terminal directly and never depend on a global active WorkUnit:

`output-append` appends the handle body and `output-replace` replaces that body. `output-status`
sets separately rendered transient status text; it does not erase the body. `output-progress` is
separate bounded progress metadata. All of these effects are stack-neutral.

An output handle belongs to the ProgramRun that opened it (including that run's verified
yield/approval resumptions). A later submission cannot reuse it; the host rejects stale,
completed, or cross-run handles.

```forth
: download-status ( S -- S ! infer )
  s" download" output-open
  dup s" starting" output-status
  dup 2 5 output-progress
  output-complete
;
```

Typed conditionals use `if ... else ... then`. Typed loops use `begin ... while ... repeat` or
`begin ... until`. Structural words must be properly nested. The condition is `bool`; the
stack/type shape after consuming it and at every back-edge must equal the loop-header shape. Each
iteration consumes fuel and observes cancellation. `do ... loop` is reserved for a later version.

Named loop exits are structured, never arbitrary jumps. Spell a named loop as `begin: label` and
target it with `break label` or `continue label` after its `while` has established an exit:

```forth
0 begin: search
  dup 3 < while
  1 + dup 2 = if break search then
repeat
```

An exit must name an active loop and leave exactly that loop's header stack row. This first
version carries no extra break values; expression-valued loop exits and `match`/`case` are later
structured forms.

Quotations are typed executable values. A quotation is code; an escaping quotation plus its captured
environment is a closure. Runtime quotations are not macros. Macro/immediate expansion occurs in a
separate bounded compile-time phase and the expanded program is reverified.

The current surface form for a quotation reference is `['] word execute`; `word` must be a
persistent typed definition and its declared stack signature supplies the quotation type. For
example, `9 ['] square execute` applies `square` to `9`. Anonymous quotations and captured Co-Forth
environments remain a later revision; Lisp lambdas already provide typed lexical closures.

Use only words advertised by `get_vm_state`. Treat any other word as unavailable regardless of
examples or prior sessions. Capability-bearing words contribute their structured requirement to the
enclosing definition; a definition cannot claim `{}` while calling one.
