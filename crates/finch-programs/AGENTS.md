# programs capsule: program identity, catalog, and corpus

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-programs/`: the durable identity and metadata of a stored program, the script
envelope (`parse_finch_script`), the wire corpus capture/audit and its source-only
`ProgramCompilerContext`, and Co-Forth token helpers.
The typed machine that runs a program is `crates/finch-vm/`; the service that schedules and
authorizes one is [`finch-runtime`](../finch-runtime/AGENTS.md).

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `crates/finch-programs/src/lib.rs` is the whole public
surface. The repository currently has no `scripts/check_subsystems.py`; facade shape is checked by
interface generation and review.

**Dependencies:** `finch-vm` supplies execution contracts, `finch-language` supplies source
compilation, and `finch-tools-api` supplies the shared `ExecutionEffect` vocabulary. Corpus capture
accepts a lazy context supplier; the application runtime may construct that context, but this
subsystem never knows or clones a `ProgramRuntime` when capture is disabled. Wire-failure
classification is VM compiler-boundary behavior re-exported here only for compatibility.

**Language is not decided here.** `ProgramLanguage` lives in `vm` and is re-exported for
compatibility; a program's meaning belongs to the VM frontends, never to a second evaluator here.

**Focused tests:** `cargo test -p finch-programs`. (`src/poset/`
looks adjacent but belongs to `runtime`.) Run the full
suite when changing a re-exported `pub` item, the script envelope, or the corpus format, because
the CLI, runtime, and live parity tests consume them.
