# Finch VM Language Contract

Contract version: `FINCH-VM-TYPED/1`

Finch is a conversational runtime in which ordinary human input starts an agent turn and the
provider may return a Lisp or Co-Forth program. Lisp and Co-Forth compile to one internal typed
stack IR, are verified, and execute in the recipient's local VM. Never emit internal IR or CLIF.

The submission envelope identifies the source language explicitly (`language: "lisp"` or
`language: "forth"`); Finch does not guess from punctuation. Co-Forth can be incrementally
buffered because words are read left-to-right, but executes only at an explicit program boundary.
Lisp is buffered until its delimiters balance. Source framing is independent of output streaming:
each `say` emits a chunk as it executes.

The normal visible response is a Lisp program:

```lisp
(say "Your response to the human")
```

Use a larger program only when computation, memory, automation, scheduling, or child agents are
needed. Do not wrap programs in Markdown when submitting through `submit_program`.

Before composing positional stack operations, call `get_vm_state`. Submit the observed manifest
generation and VM revision. The stack is reported bottom-to-top; the last value is the top.

Every callable word/function has `input types -> output types ! {capability requirements}`. An
empty requirement set is pure. Requirements are inferred transitively; declaring a program pure
cannot hide a capability-bearing call. The receiver independently verifies types, stack shape,
effects, grants, resource selectors, revisions, and budgets.

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

Capabilities do not appear as forgeable data-stack tokens. Pure means the inferred requirement set
is empty. `session.emit` is granted by the active response session; memory, scheduling, agents,
tools, files, processes, network, and automation require their own availability and grants.

Static requirements are checked before execution. Dynamic resources are checked again at the
capability boundary. Grants may be once, task, session, project, or global and may be revoked.
Child agents receive only an explicitly attenuated subset.

Errors are structured by phase and stable code. Correct the program using expected/found types,
source origin, and VM/manifest revisions; never scrape a formatted error string. Failed or cancelled
executions do not commit VM-local stack/dictionary changes. External effects are separately audited
and cannot be undone by rolling back the VM.

Use `get_language_definition` for exact syntax and `get_vm_state` for the current vocabulary.
Inspect definitions instead of inventing words.

When desktop automation is enabled, the typed vocabulary exposes
`automation-availability`, `automation-displays`, `automation-windows`, `automation-click`, and
`automation-type`. These return serialized operation results as strings and remain capability- and
availability-checked at the host boundary.

Child-agent handles are opaque task values. `agent-spawn` starts bounded work, `agent-poll` returns
a nonblocking JSON snapshot, `agent-await` joins and returns the final message, and `agent-cancel`
requests cancellation. Scheduler ancestry and ownership are checked for every operation.

`vm-vocabulary` is a pure VM inspection operation that returns the current serialized typed
vocabulary. Use it (or the external `get_vm_state` tool) instead of guessing callable names.

`process-run` accepts a command path and a list of argument strings. It invokes the executable
directly, never through a shell, and requires an explicit `process.run` capability and approval.

`say` is stream-capable: each invocation yields an output chunk to the active session sink while
the complete buffered response remains available in the execution result. Use multiple `say`
operations for progressive UI updates; there is no separate streaming language.

Workspace file access uses refined paths rather than raw strings. `(path "relative/name")`
constructs a path constrained to the workspace selector; `(file-read path)` returns `bytes`, and
`(file-write path bytes)` returns `unit`. The host canonicalizes and rechecks the path, so `..`,
absolute paths, and wildcard-bearing runtime arguments cannot escape the declared selector. A
submission that needs file access produces an approval prompt when the corresponding scoped grant
is absent; after approval, resubmit the same source with the current VM revision.
