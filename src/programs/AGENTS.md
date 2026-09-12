# programs capsule: program identity, catalog, and corpus

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/programs/`: the durable identity and metadata of a stored program, the script
envelope (`parse_finch_script`), the wire corpus capture and audit, and Co-Forth token helpers.
The typed machine that runs a program is `src/vm/`; the service that schedules and authorizes one
is `src/runtime/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/programs/mod.rs` is the whole public surface,
and `scripts/check_subsystems.py` rejects a `pub mod` there.

**Dependencies:** layer 1 in [`subsystems.toml`](../../subsystems.toml), may depend on `vm` only.
Debt: `config` (via `crate::metrics`) and `runtime`. Importing any other subsystem fails
`scripts/check_subsystems.py`, and the check will not stop a new `config` or `runtime` import, so
add none.

**Language is not decided here.** `ProgramLanguage` lives in `vm` and is re-exported for
compatibility; a program's meaning belongs to the VM frontends, never to a second evaluator here.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- programs::`. (`src/poset/`
looks adjacent but belongs to `runtime`.) Run the full
suite when changing a re-exported `pub` item, the script envelope, or the corpus format, because
the CLI, runtime, and live parity tests consume them.
