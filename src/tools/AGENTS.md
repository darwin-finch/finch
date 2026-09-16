# tools capsule: tool execution, permissions, and local implementations

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/tools/` except `mcp`: the executor, registry, permission policy, persistent
approval patterns, session task list, the event-loop-owned [`ToolLoop`](tool_loop.rs)
protocol, the semantic advertisement catalog (`semantic.rs`, issue #241), and the
local tool implementations. Connecting to external Model Context Protocol servers
is the nested [`mcp`](mcp/AGENTS.md) capsule.

**ToolLoop is the single execution lifecycle.** REPL and scheduler drive it.
Generators and provider adapters never import or invoke `ToolExecutor`.
Malformed arguments, duplicate ids, unknown tools, and unsupported tools fail
closed with a typed result and never execute. Cancel, timeout, disconnect,
retry, and late-result-after-terminal admit at most one execution and append
at most one result.

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
surface as AskUser. See the root [Security invariant](../../CLAUDE.md#security). The peer
hard-deny and allow tables are keyed on registered tool names (`PEER_HARD_DENY_TOOLS`,
`PEER_SILENT_ALLOW_TOOLS`, `PEER_REVIEWED_CHANGESET_TOOLS`, `VM_DISCOVERY_TOOLS`); conformance
tests in this subtree and in `src/cli/repl/always_allow_tests.rs` fail if a policy table names
anything no `Tool` registers or alias key covers.

**Effects are declared, not guessed (issue #466).** Every `Tool` implements `fn effect(&self) ->
ExecutionEffect` with no default, so a new tool cannot exist without stating its authority and a
rename carries the declaration with it. There is no string-keyed effect table left in this
subtree: approval call sites read `ToolRegistry::declared_effect(name)` (alias-resolved,
Unclassified for names nothing registers) instead of classifying by literal. A declaration is the
tool's **worst case**; input-dependent refinements live at the approval sites that consume the
effect — `refined_effect_for_approval` applies bash's read-only refinement and pins its literal
to the name `BashTool` registers. Deleting a tool's `effect()` is a compile error; changing one
must trip `test_declared_effects_match_pre_refactor_classification` in
`src/cli/repl/always_allow_tests.rs`, which pins every registered tool's declaration to its
pre-refactor classification. The planning allowlists (`PLANNING_MODE_ALLOWED_TOOLS` in
`src/cli/repl_event/plan_handler.rs`, `REPL_PLANNING_ALLOWED_TOOLS` in `src/cli/repl.rs`,
`EXECUTOR_PLANNING_ALLOWED_TOOLS` in `permissions.rs`) are keyed on registered names or alias
keys and are conformance-tested in the same file; spellings nothing registers (`ExitPlanMode`,
`Bash`) are deliberately blocked. Declaring `Unclassified` is a real decision: todo_read,
todo_write, enter_plan_mode, present_plan, ask_user_question, inspect_memory, and the four agent
tools preserve their pre-refactor approval behavior that way, and re-authorizing any of them is a
deliberate approval-policy change with its own review, not a drive-by declaration edit.

**A tool name is not an instruction.** Implementations receive model-supplied names, paths, and
schemas as data. MCP names are namespaced before they reach the registry; do not invent a second
permission path around that.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- tools::`. That includes the
security tests in `permissions.rs`. Run the full suite when changing a re-exported `pub` item or
the permission policy, because the CLI, runtime, scheduler, and providers all hold these types.
