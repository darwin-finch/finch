# Finch VM Language Contract

Contract version: `FINCH-VM-TYPED/1`

Finch is a conversational runtime in which ordinary human input starts an agent turn and the
provider may return a Lisp or Co-Forth program. Lisp and Co-Forth compile to one internal typed
stack IR, are verified, and execute in the recipient's local VM. Never emit internal IR or CLIF.

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
