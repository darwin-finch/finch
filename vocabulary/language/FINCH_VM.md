# Finch VM Language Contract

Contract version: `FINCH-VM-TYPED/1`

Finch is a conversational runtime in which ordinary human input starts an agent turn and the
provider may return a Lisp or Co-Forth program through the direct response-wire boundary or a
provider-native program tool. Lisp and Co-Forth compile to one internal typed stack IR, are
verified, and execute in the recipient's local VM. A text-only provider response is source, not
display prose: use `say` for human-visible language. Shell stdout remains display content and is
never executable VM input. Never emit internal IR or CLIF.

The submission envelope may identify the source language explicitly (`language: "lisp"` or
`language: "forth"`). For compact tool calls that omit it, Finch infers Lisp only when the first
non-whitespace character is `(` and otherwise treats the source as Forth; the resolved language
is recorded before execution. Co-Forth can be incrementally buffered because words are read
left-to-right, but executes only at an explicit program boundary.
Lisp is buffered until its delimiters balance. Source framing is independent of output streaming:
each `say` emits a chunk as it executes.

The receiving host, not the model, attaches a stable source identity to every submission (for
example a script path, provider response ID, scheduled callback, or editor buffer). Diagnostics and
effect origins preserve that identity through lowering and suspension/resume; a program cannot
forge its own provenance by placing a string in source text.

The normal visible response is a Lisp program:

```lisp
(say "Your response to the human")
```

Prefer Lisp for model-authored programs with branching, bindings, closures, or more than a tiny
stack pipeline. Co-Forth is the compact, incrementally bufferable alternative:

```forth
s"Your response to the human" say
```

Use a larger program only when computation, memory, automation, scheduling, or child agents are
needed. Do not wrap programs in Markdown when submitting through `submit_program`.

Before composing positional stack operations, call `get_vm_state`. Submit the observed manifest
generation and VM revision. The stack is reported bottom-to-top; the last value is the top.

Every callable word/function has `input types -> output types ! {capability requirements}`. An
empty requirement set is pure. Requirements are inferred transitively; declaring a program pure
cannot hide a capability-bearing call. The receiver independently verifies types, stack shape,
effects, grants, resource selectors, revisions, and budgets.

Structured option branches are part of the common semantics: Co-Forth `if-some ... else ... then`
and Lisp `(match-option option (some name ...) (none ...))` both lower to typed branch edges. The
some edge receives `T`, the none edge consumes the option, and the two edges must merge with the
same verified stack/value type; no dynamic type test or thrown `unwrap` is needed for ordinary
option control flow.

Structured result branches use the same typed control-flow rule: Co-Forth
`if-ok ... else ... then` gives the `ok` payload to its then edge and the `err` payload to its
else edge; Lisp `(match-result result (ok name ...) (err name ...))` binds the corresponding
payload lexically. Both arms are required and must merge with the same verified stack/value type.
Lisp may use the type-directed `(match value arms...)` spelling for these two exhaustive tagged
forms; it selects the same lowering and never falls back to dynamic pattern dispatch.

Co-Forth also supports an integer `case` with `of ... endof` arms and an optional `otherwise`.
It lowers to the same verified branch edges, has no fallthrough, and requires every reachable arm
to leave the same stack row. It is not a dynamic dictionary dispatch or C-style switch.

Capabilities are positive, scoped grants such as:

```text
fs.read(${workspace}/src/**)
fs.write(${workspace}/generated/**)
memory.read(tree=session,path=**)
agent.spawn(max_depth=2,max_children=4)
schedule.create(policy=daily-report)
session.emit
```

Capability parameters compose through calls. Argument-dependent filesystem requirements use a
restricted template AST over immutable refined `path` values, never string replacement. Each
template has a conservative upper bound (for example `${workspace}/reports/**`); Finch substitutes
the actual argument at a call site and checks the canonical resolved path again at the host
boundary. If it cannot prove the result remains within the bound, verification fails.

Response/UI effects are stack-neutral in the shared IR. `say` and `output-*` consume their
arguments and leave no synthetic `unit` on the persistent stack; consecutive calls compose without
`drop`. The Lisp frontend represents effect-only forms as `unit` internally so they remain valid
expressions, then removes that implementation detail at its top-level boundary.

Capabilities do not appear as forgeable data-stack tokens. Pure means the inferred requirement set
is empty. `session.emit` is granted by the active response session; memory, scheduling, agents,
tools, files, processes, network, and automation require their own availability and grants.

Static requirements are inferred and exposed before execution for declaration checks and permission
preview. A missing grant pauses only when execution reaches the concrete capability boundary, so
earlier pure work and streamed `say` output are not discarded because an unreachable branch has an
effect. Dynamic resources are checked again at that boundary. Grants may be once, task, session,
project, or global and may be revoked. Child agents receive only an explicitly attenuated subset.

Errors are structured by phase and stable code. Correct the program using expected/found types,
source origin, and VM/manifest revisions; never scrape a formatted error string. Failed or cancelled
executions do not commit VM-local stack/dictionary changes. External effects are separately audited
and cannot be undone by rolling back the VM.

Use `get_language_definition` for complete grammar, `get_vm_state` for the current stack/revision,
and `search_word` followed by `inspect_word` for a core word, persisted definition, **or source
syntax** such as `while`/`define`. Source syntax is deliberately reported without a pretend runtime
signature. Inspect definitions instead of inventing words.

When desktop automation is enabled, the typed vocabulary exposes
`automation-availability`, `automation-displays`, `automation-windows`, `automation-click`, and
`automation-type`. These return serialized operation results as strings and remain capability- and
availability-checked at the host boundary.

Child-agent handles are opaque, persistent `task<string>` values: they carry a daemon task ID and
their declared terminal type, not a process-local future. A handle may remain on the VM stack
across later program turns. `agent-spawn` starts bounded work, `agent-poll` returns a nonblocking
JSON snapshot, `agent-await` joins and returns the final message, and `agent-cancel` requests
cancellation. Scheduler ancestry and ownership are checked for every operation.

Pure zero-argument closures may use `(defer :cpu (lambda () ...))` to run on Finch's bounded local
worker pool. It returns `task<T>` with an explicit `cpu_fiber` owner kind; captured values are
immutable snapshots and the worker has a private stack/frame set. `task-poll` has type `task<T> -> option<T>` and never
blocks. `task-join` has type `task<T> -> T`; if the worker is still running it preserves the parent
continuation as a scheduler suspension instead of blocking the UI/event loop. `task-cancel`
consumes a CPU task handle and requests cooperative cancellation at the worker's next VM boundary.
CPU task operations reject agent-task handles; agent coordination remains `agent-*` and is a
separate protocol.

`yield` is a stack-neutral control instruction. It returns the VM's remaining frames to the Finch
event-loop trampoline as a saved `ProgramRun` suspension. The interactive provider-wire runner
currently yields its Tokio task and resumes that exact execution automatically; all other hosts
choose their own scheduling policy. It may occur more than once in one program; it is not an
LLM-authored continuation value. Future `fiber<yield,return>`
handles will expose repeated yielded values separately from terminal task joins.

It is not a lazy sequence generator. This revision has no `next`, `generator<T>`, or generic
iterator protocol; do not borrow such words from the legacy Co-Forth compatibility library. Known
finite values use typed lists and host-backed iteration uses explicit cursor resources.

`vm-vocabulary` is a pure VM inspection operation that returns the current serialized typed
vocabulary. Use it (or the external `get_vm_state` tool) instead of guessing callable names.

Structured values are part of the shared ABI. `some` and `none` construct `option<T>` values;
`is-some` tests one and `unwrap` extracts its payload (returning a structured `E-OPTION-001`
diagnostic for `none`). `ok` and `err` construct `result<T,E>` values, `is-ok` inspects which
branch was produced, and `result-unwrap`/`result-error` project the corresponding payload with a
structured diagnostic on the wrong branch. These values remain typed when crossing the VM/runtime boundary and
are rendered to legacy Lisp callers as `(some value)`, `(none)`, `(ok value)`, or `(err value)`.

`json-parse` is pure and returns `result<json,string>` rather than throwing on untrusted input.
`json-get` takes a managed `json` value and a string field name, returning `option<json>`; it only
looks up object fields and never treats a supplied string as code, a path, or authority. Convert a
known scalar through `json-as-string`, `json-as-int`, `json-as-float`, or `json-as-bool`, each of which returns an
`option` rather than coercing or guessing. `json-index` similarly returns an option for an array
element, and `json-keys` returns a typed `list<string>` (empty for a non-object). `json-stringify`
explicitly returns compact JSON text.

Typed maps and records are also shared source values, distinct from arbitrary `json`. Maps accept
typed keys and values: Lisp spells construction `(map key value ...)`, while Co-Forth spells
`map{ key value ... }map`. `map-get` returns `option<V>`, `map-set` returns a replacement map, and
`map-entries` returns an insertion-ordered `list<record{key:K,value:V}>`. String-keyed maps are
the appropriate normalized representation for external keys such as `"first name"`.

Typed records are immutable heterogeneous products with statically known identifier-like field
names: Lisp `{ :name value ... }`; Co-Forth `{ name: value ... }`. `(record-get record "name")`
and `record "name" record-get` return `option<T>`. `(record-set record "name" value)` and
`record value "name" record-set` return a replacement record after verifying the literal field
name and replacement type. A record may hold a typed closure; method invocation remains explicit
and never grants ambient mutable `self` access. Arbitrary JSON object keys—including keys with
spaces—remain at the `json-get` or string-keyed-map boundary rather than being silently coerced
into typed record fields.

Typed lists are homogeneous immutable values: Lisp spells construction `(list value ...)` and
Co-Forth spells `list{ value ... }list`. `list-append` consumes a list and matching element and
returns a replacement list; `list-get` and `list-length` inspect it. Empty list literals require
an explicit element type and are therefore intentionally not inferred by either source form.

Lisp symbols are identifiers, not strings: `'name` is quoted data (equivalent to `(quote name)`),
whereas `(say "name")` contains text. Co-Forth uses bare tokens for executable dictionary words;
`'name` produces a typed symbol value and `['] word execute` passes an execution token as data.
These remain distinct from both a dictionary word reference and a string.

`process-run` accepts a command path and a list of argument strings. It invokes the executable
directly, never through a shell, and requires an explicit `process.run` capability and approval.

`say` is stream-capable: each invocation yields an output chunk to the active session sink while
the complete buffered response remains available in the execution result. It appends its text
exactly: it adds neither a space nor a newline. Use multiple `say` operations for progressive UI
updates only, and include separators in the text yourself (for example,
`s"result: " say 2 3 + int-to-string say s"\n" say`). There is no separate streaming language.

At the execution boundary, chunks are represented as ordered typed side-effect events. A session
may render, buffer, inspect, test, or reject those events; the VM does not require the UI to mutate
state synchronously during interpretation.

The portable event envelope contains the VM protocol version, monotonically increasing sequence,
capability requirement, typed event payload, expected typed result row (empty for terminal emits),
and source origin. A harness pairs the sequence with
its execution ID as an idempotency key, records the result in its effect journal, validates the
typed result against the awaited output row, then resumes the serialized VM continuation. A
`VmResume { execution_id, sequence, response }` acknowledges that exact event and does not
dispatch the host operation a second time. Its response is exactly one of
`result { values }`, `denied { reason }`, or `cancelled { reason? }`; a stale sequence is rejected
without consuming a newer continuation. The terminal effect-journal state records the same choice
as `acknowledged`, `denied`, or `cancelled`.
Per-run effect observers receive an awaited request before authorization or local dispatch, so a
host can persist or route that boundary without waiting for a synchronous adapter.
An embedder may choose the portable deferred-host mode, in which every approved awaited request
(file, process, network, proposal, and so on) suspends and must receive this correlated
`VmResume`; Finch's compatibility submission mode still dispatches its installed host bindings
synchronously except for editor proposals.
Finch projects these events onto its reactive `WorkUnit`/shadow-buffer
UI; another harness may instead use a browser, IDE, voice interface, or audit log. `say` is only a
durable response-append event, never a direct terminal write. Rich replaceable/progress output will
use host-issued opaque output handles rather than guessed string or symbol IDs.

Editor-backed proposals are language-neutral host operations, not a property of Forth. An explicit
Lisp/control-plane proposal may carry Bash, Python, Lisp, Forth, or another supported script:

The outer event loop owns the editor and returns `execute`, `chat`, or `cancel`. `execute` submits
the edited payload through that language's normal validator and capability boundary; `chat` returns
the edited buffer as context without running it; `cancel` discards it. Ordinary Forth execution
never opens an editor implicitly.

The current typed surface spells this as three strings so it remains equally available in both
frontends: `(proposal-open "bash" "Regenerate local indexes" "#!/usr/bin/env bash\\n...")` or
`s"bash" s"Regenerate local indexes" s"#!/usr/bin/env bash\\n..." proposal-open`. It requires
`program.invoke` and returns `option<result<string,string>>`: `none` means cancel,
`some(ok source)` means accepted artifact source, and `some(err context)` means chat. Acceptance
returns data only; it never executes the proposal. A Finch payload must be submitted again through
the typed verifier, while an external script follows the separately authorized script workflow.
The requirement is concrete at the call boundary (`program.invoke(language="python")`, for
example), so granting one artifact language does not confer authority to open another.

Workspace file access uses refined paths rather than raw strings. `(path "relative/name")`
constructs a path constrained to the workspace selector; `(file-read path)` returns `bytes`, and
`(file-write path bytes)` returns `unit`. The host canonicalizes and rechecks the path, so `..`,
absolute paths, and wildcard-bearing runtime arguments cannot escape the declared selector. A
submission that needs file access produces an approval prompt when the corresponding scoped grant
is absent; after approval, Finch resumes the saved execution ID and its verified frame. Do not
resubmit source: doing so would repeat any prior `say` output or completed host effects.

Whole-machine scope is explicit rather than an absolute-string escape hatch. A host may install
`root<host-machine>` (normally a narrower directory; `/` only after an intentional full-machine
decision). `(host-path "relative/name")` produces the distinct `path<host-machine>` type, which
only `host-file-read` and `host-file-write` accept. Installing the root is availability, not a
grant: the concrete `file.read(${host-machine}/...)` or `file.write(${host-machine}/...)`
capability must still be approved, and the host canonicalizes containment again on every call.
Ordinary `path` / `file-read` never widen to this root.

`file-size(path)` and `file-slice(path, offset, length)` share the same refined `file.read(path)`
requirement. A slice is bounded (currently 8 MiB maximum per call) and may be shorter at EOF, so
large CSV/text/binary processing can keep only a bounded window in VM memory.
`file-lines-open(path)` mints a ProgramRun-owned opaque `stream<string>`; `stream-next`
returns bounded UTF-8 `option<string>` records (1 MiB maximum line) and `stream-close`
releases the stream. The initial path grant covers later stream operations only because the stream
is unforgeable and host-validated on each call. `file-lines-next` and `file-lines-close` remain
compatibility aliases.
`csv-open(path)` mints a distinct `stream<list<string>>`; `stream-next` returns bounded
`option<list<string>>` CSV records and `stream-close` releases it. It parses quoted,
comma-containing, and multiline fields rather than treating physical lines as records; malformed
quote boundaries fail with a host diagnostic. Both stream kinds use the same opaque-handle pattern;
workbook resources must follow it too and must not smuggle an ambient filesystem path back into VM
values.
