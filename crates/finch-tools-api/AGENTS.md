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
`finch-programs`; `VmEffectEnvelope`/`VmEffectHandle` are defined here and re-exported by
`finch-runtime`. Permission and approval types are the same values the runtime, CLI, and scheduler
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

**Surface tiers (issue #962 audit).** The `pub use` list in [`src/lib.rs`](src/lib.rs) is the
cross-crate contract; everything below it is tiered so implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** `resolve_canonical_path`,
  `ToolSignature::full_command`, `PermissionManager::allows_advertising`, and every
  `ToolPatternMatcher` method (`new`, `with_default_patterns`, `extract_tool_uses`,
  `matches_any`). Widening any of these is a capsule change, not cleanup.
- **Module-internal (private):** `path_is_inside_workspace`,
  `path_argument_escapes_workspace`, `ExecutorRole` (the role is chosen through
  `PermissionManager::new`/`for_peer`, never set by callers),
  `ExactApproval::{matches, increment_match}`, `RejectReason::typed_message`.
- **Test-only (`#[cfg(test)]`):** `PersistentPatternStore::{get_pattern, find_by_id,
  find_by_id_mut}`, `ToolPattern::new_structured`, `PermissionManager::workspace_root`,
  `ToolRegistry::{len, dispatch_names}`, `ToolCatalog::new`,
  `ToolLoop::{identity, execution_starts, results_appended}`, and the five `ContentBlock`
  accessors (`is_text`, `is_tool_use`, `is_tool_result`, `as_text`, `as_tool_use` — callers
  match on the enum).

`ToolPatternMatcher` itself stays exported as the retained pre-neural-selector capability even
though no subsystem calls it yet; its module carries a scoped
`cfg_attr(not(test), allow(dead_code))` for exactly that reason, and the same scoped allow on
`ToolLoop.identity` documents the field's pinned-for-the-round role while its accessor is
test-only. Deleted as unreferenced by the same audit: `PersistentPatternStore::{get_exact,
prune_unused}`, `ToolPattern::increment_match` (deprecated alias of `record_match`),
`PermissionManager::{from_config, with_max_turns}`, `ToolRegistry::is_empty`, and
`ToolLoop::{is_terminal, terminal}`.

**Boundary:** the [README](README.md) traces tool execution and REPL tool-round callers;
[`src/lib.rs`](src/lib.rs) is the flat facade. `cargo doc -p finch-tools-api --no-deps --open`
renders methods on re-exported types. Child modules remain private; do not regenerate a
signature catalog.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -p finch-tools-api`.
