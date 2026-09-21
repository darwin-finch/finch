# Finch tool API

`finch-tools-api` defines the shared contract for a tool call: declarations, registry, typed
requests and results, permission and approval policy, live output, and the exactly-once
tool-round state machine. It deliberately has no concrete tool implementation, executor, UI, or
application session. Those belong at the root composition layer.

Two callers show why the contract sits below the application:

1. The [tool executor](../../src/tools/executor.rs) receives a `ToolRegistry` and
   `PermissionManager`, builds a per-call `ToolContext` with injected host state, and invokes a
   registered tool. The API owns effect and permission meaning; the executor owns concrete
   dispatch, application ports, and result wiring.
2. The [REPL tool coordinator](../../src/cli/repl_event/tool_execution.rs) holds a `ToolLoop`
   for each query and records terminal results once. The API owns admission and terminal-state
   rules; the REPL owns event ordering, presentation, and cancellation delivery.

The [agent contract](AGENTS.md) states authority and dependency invariants. The [flat
facade](src/lib.rs) and `cargo doc -p finch-tools-api --no-deps --open` show the callable API.
