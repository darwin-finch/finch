# Finch typed VM

`finch-vm` executes already verified modules and provides the application-facing compatibility
surface for the shared typed-machine contracts. It owns interpreter steps, typed runtime state,
fibers, suspension, and checkpoints. It does not choose a source-language frontend, decide host
authority, or implement application persistence.

Two callers use different parts of that surface:

1. The [program runtime](../finch-runtime/src/lib.rs) creates or restores a `TypedRuntime`,
   submits a verified module with host bindings, and handles typed suspensions. The VM preserves
   execution and checkpoint semantics; `finch-runtime` binds grants, audit, and host effects.
2. The [programs crate](../finch-programs/src/lib.rs) uses `wire_diagnostic_code` and
   `classify_wire_failure` to group rejected provider submissions without retaining source or
   diagnostic prose in aggregate metrics. This compatibility seam classifies compile-boundary
   failures; it must not turn a host-effect failure into an automatic retry.

The [agent contract](AGENTS.md) gives ABI and dependency rules. The [flat facade](src/lib.rs) and
`cargo doc -p finch-vm --no-deps --open` show the callable API. Source compilation belongs to
[finch-language](../finch-language/README.md), not this crate.
