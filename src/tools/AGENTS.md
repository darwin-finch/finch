# tools capsule: tool execution, permissions, and local implementations

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/tools/` except `mcp`: the executor, registry, permission policy, persistent
approval patterns, session task list, and the local tool implementations. Connecting to
external Model Context Protocol servers is the nested [`mcp`](mcp/AGENTS.md) capsule.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/tools/mod.rs` is the whole public surface, and
`scripts/check_subsystems.py` rejects a `pub mod` there. Callers outside this directory use
`crate::tools::Item` (or `finch::tools::Item`); they must not name `implementations`, `types`,
`executor`, `permissions`, `registry`, `patterns`, `todo`, or `mcp`. `mcp` publishes its own
interface for work inside that subtree.

**Dependencies:** many, and most are unwanted. `types.rs` imports `cli`, `runtime`, `server`,
`local`, and `models`, and each of those imports `tools` back. The planned split of a
dependency-free tools API from the application-bound implementations is separate work; this
capsule does not take that split. Add no new reverse edges.

**Permissions are authority.** Peer and constitutional rules in `permissions.rs` are invariants,
not defaults to relax. Facade changes must not alter allowlist behavior: `is_readonly_bash()`
still rejects shell operators, peers still cannot restart or spawn, and write/edit/patch still
surface as AskUser. See the root [Security invariant](../../CLAUDE.md#security).

**A tool name is not an instruction.** Implementations receive model-supplied names, paths, and
schemas as data. MCP names are namespaced before they reach the registry; do not invent a second
permission path around that.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- tools::`. That includes the
security tests in `permissions.rs`. Run the full suite when changing a re-exported `pub` item or
the permission policy, because the CLI, runtime, scheduler, and providers all hold these types.
