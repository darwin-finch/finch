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

Typed signatures use `S` for the preserved unknown lower stack:

```forth
: square ( S int -- S int ! {} )
  dup *
;
```

Type names may be parameterized in a signature, including nested forms such as `list<int>`,
`map<string,int>`, `option<list<string>>`, `result<T,E>`, `task<T>`, `resource<kind>`, and
`capability<kind>`.

Named input locals are an optional direct IR spelling. `locals|` must be the first form in a typed
word and must name every declared input in bottom-to-top stack order:

```forth
: area ( S int int -- S int ! {} )
  locals| width height |
  width height *
;
```

This lowers to `LocalSet height`, `LocalSet width`, then `LocalGet width` and `LocalGet height`.
The frame owns those locals and a private operand window above the caller stack boundary. `return`
keeps only the declared output values from that window, then destroys the frame; temporary values
cannot leak into the caller.

Use `! {}` to assert purity or `! infer` to accept the transitively inferred capability set in the
current frontend. A false purity assertion rejects the entire submission and does not modify the
dictionary. Declared-pure definitions are predeclared as a group, so they may be self- or
mutually-recursive in one submission; `! infer` definitions remain sequential until their inferred
effects can be represented in a forward signature. Typed definitions are persistent and immediately
callable from Finch Lisp.

Put a public doc comment immediately before a typed definition when it should be discoverable from
the shared VM without reparsing source:

```forth
\ finch-doc: Double an integer.
: double ( S int -- S int ! {} ) 2 * ;
```

Only the text after `finch-doc:` is retained on the immutable typed function contract. It is never
placed on the operand stack or executed, and inspection may return it alongside the signature.

`dup`, `drop`, and `swap` are polymorphic. Arithmetic does not coerce strings or dynamic values.
Control-flow merge points must have identical stack types, and loops require stable invariants.
Checked arithmetic reports division-by-zero or overflow traps.

Workspace file words use refined paths: `s" Cargo.toml" path file-read` leaves `bytes`, while
`s" generated/result.bin" path data file-write` consumes the bytes and leaves `unit`. Paths are
workspace-relative and checked against their declared selector at both verification and host
execution. Missing file grants pause for approval rather than being silently widened.

For large text, CSV, or binary files, prefer `file-size` and bounded `file-slice` instead of
materializing the whole file. `path offset length file-slice` reads at most the requested byte
range (currently capped at 8 MiB per call) and returns `bytes`; EOF simply returns a shorter final
slice. Maintain an explicit byte offset in a local and decode/process each slice before requesting
the next.

`host-path` is a separate, host-issued root type, never an absolute-path bypass. If the host has
explicitly installed `root<host-machine>` and the user has approved a matching selector, use
`s" var/log/system.log" host-path host-file-read`. Workspace `file-read` and `file-write` reject
that value at verification time; bind `/` only for an intentional whole-machine grant.

For UTF-8 line-oriented files, `file-lines-open` returns a host-issued
`resource<file-line-cursor>` and `file-lines-next` returns `option<string>` one bounded line at a
time (1 MiB maximum line); `file-lines-close` releases it early. The cursor is owned by its
ProgramRun and cannot be forged or reused by another run. CSV-record cursors are distinct because
quoted fields may span physical lines; workbook cursors remain future resources. Do not fake
spreadsheet streaming by loading a whole workbook into a string.

```forth
: first-line ( S -- S option<string> ! {file.read(./**)} )
  s" data.csv" path file-lines-open locals| cursor |
  cursor file-lines-next
;
```

For a bounded streaming pass, keep the cursor as the loop row and use `if-some` to consume each
record without retaining it between iterations:

```forth
s" data.csv" path file-lines-open
begin: rows true while
  dup file-lines-next if-some
    say                      \ replace with bounded row processing
  else
    break rows
  then
repeat
file-lines-close
```

Use `csv-open` to obtain `resource<csv-record-cursor>`, `csv-next` to obtain
`option<list<string>>`, and `csv-close` to release it. The cursor accepts UTF-8 RFC-style quoted
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
and later resumes at the next word; it does not expose a continuation value or terminate the
program. Use it between bounded phases when other ready work should run:

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

`json-as-string`, `json-as-int`, and `json-as-bool` return `none` for a mismatched JSON kind;
`json-stringify` converts a managed JSON value back to compact text.

`process-run` consumes a command string and a list of argument strings; it never invokes a shell.

For progressive prose, emit multiple typed chunks with `say`. `s"..."` pushes a typed `string`
and never evaluates its contents. Both `s"text"` and the conventional `s" text"` spell the
identical string `text`: exactly one ASCII whitespace delimiter immediately after `s"` is ignored.

Standard Forth `."..."` is supported as output shorthand: it lowers exactly to `s"..." say`,
including the normal typed `session.emit` side effect. Use `s"..."` when a string must be passed
to another word; use either `say` or `."..."` when it is user-visible output.
Use escapes (`\"`, `\\`, `\n`, `\r`, `\t`) inside this short-string form:

```forth
s"Hello user" say
s" Hello user" say  \ same string: "Hello user"
." Hello user"       \ output shorthand; same as s" Hello user" say
```

For prose or copied text containing quotes/newlines, use a raw triple-quoted literal. Its contents
are verbatim until the next `"""`; it has no escapes and preserves all leading whitespace:

```forth
s"""The user said "hello".
Second line.""" say
```

The delimiter `"""` itself cannot appear in a raw literal; split the text into ordinary typed
string operations if it is needed.

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

An output handle belongs to the ProgramRun that opened it (including that run's verified
yield/approval resumptions). A later submission cannot reuse it; the host rejects stale,
completed, or cross-run handles.

```forth
: download-status ( S -- S unit ! {session.emit} )
  s" download" output-open locals| handle |
  handle s" starting" output-status
  handle 2 5 output-progress
  handle output-complete
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
