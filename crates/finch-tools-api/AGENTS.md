# finch-tools-api capsule: the dependency-free tool surface

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** the tool surface every application layer shares: the [`Tool`](src/registry.rs) trait and
[`ToolRegistry`](src/registry.rs), the typed requests and results (`ToolUse`, `ToolResult`,
`ContentBlock`), the permission and approval policy (`permissions`, `patterns`), the
declared-effect vocabulary (`ExecutionEffect`), the per-call [`ToolContext`](src/types.rs) with its
injected application ports, the tool-invocation `ToolSignature`, and the event-loop-owned tool-round
protocol (`tool_loop`). It contains **no concrete tool implementations and no executor** — those stay
with the composition root in `src/tools`.

**Dependency-free by construction.** This crate may not name any root-crate subsystem: no `cli`,
`server`, `runtime`, `local`, `models`, `brain`, or `programs`. Its only workspace dependencies are
the extracted leaf crates `finch-providers` (provider wire types, re-exported unchanged) and
`finch-vm` (VM effect vocabulary). `scripts/seam_cost.py crates/finch-tools-api/` must report zero
outgoing root-crate references; anything else is a regression of this capsule.

**Types are shared, never forked.** `ExecutionEffect` is defined here and re-exported by
`src/programs`; `VmEffectEnvelope`/`VmEffectHandle` are defined here and re-exported by
`src/runtime`. Permission and approval types are the same values the runtime, CLI, and scheduler
hold. If a caller needs a different shape, extend the shared type here — do not fork it.

**Application-bound state arrives by injection.** [`ToolContext`](src/types.rs) names only what this
crate can: the host's live session mode arrives as the opaque `HostModeState` port and the
daemon-issued effect-audit authority as the `EffectAuditAuthority` port (`as_any` downcast). The
composition root injects the concrete handles (see `src/tools/executor.rs`); carriers that fail to
downcast must fail loud or degrade conservatively, never silently drop authority.

**Permissions are authority.** The peer hard-deny, silent-allow, and reviewed-changeset tables, the
constitutional denials, `is_readonly_bash`, and the bash readonly refinement moved here verbatim
from `src/tools/permissions.rs`. The pure policy tests live beside them in
`src/permissions.rs`. The tests that exercise real registered tool implementations remain at the
composition root (`src/tools/permissions/tests.rs`), at their original
`tools::permissions::tests::*` module path, because only there can the real tools be constructed.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from [`src/lib.rs`](src/lib.rs); CI fails
if it drifts. Edit the code, then run `python3 scripts/generate_interfaces.py --write`.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -p finch-tools-api`.
