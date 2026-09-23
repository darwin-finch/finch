# tools capsule: tool execution, implementations, and composition-root wiring

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/tools/` except `mcp`: the [`executor`](executor.rs), the concrete tool
implementations, the event-loop integration of the tool-round protocol, the session
task list (`todo.rs`), and the post-edit diagnostics service (`diagnostics/`, issue #757). The
tool surface these implement — the `Tool` trait, `ToolRegistry`, typed requests and results, the
permission and approval policy, `ExecutionEffect`, `ToolSignature`, and the tool-round protocol —
is defined in the dependency-free [`finch-tools-api`](../../crates/finch-tools-api) crate
(issue #872). Connecting to external Model Context Protocol servers is the nested
[`mcp`](mcp/AGENTS.md) capsule. The background command tools (`background_bash`,
`background_poll`, `background_stop`, issue #754) are thin siblings of bash over the brain-owned
`BackgroundTaskManager` lifecycle; the task records and process ownership live in `crates/finch-brain`, not
here. The diagnostics service annotates completed write/edit/patch results with bounded output
from a check command the user declared in `[diagnostics]` config — nothing is inferred, and the
declared command's authority verdict is read from the existing bash approval path
(`PermissionManager::check_tool_use("bash", …)`), so it never runs where bash would not.

**ToolLoop owns REPL and scheduler rounds.** Those two callers admit a call through the
`finch-tools-api` protocol before execution; malformed arguments, duplicate ids, unknown or
unsupported tools fail closed with a typed result. Cancel, timeout, disconnect, retry, and
late-result-after-terminal must not admit another execution or append another result. The
legacy headless `finch agent` loop calls `ToolExecutor` directly and does not have that
`ToolLoop` admission lifecycle; see the [README](README.md) before extending this path.
Generators and provider adapters must not import or invoke `ToolExecutor` themselves.

**Facade:** child modules are private, so the flat `pub use` list in [`mod.rs`](mod.rs) is the
callable surface; use rustdoc for methods, not a generated signature catalog. Callers outside this directory use
`crate::tools::Item` (or `finch::tools::Item`); they must not name `implementations`, `types`,
`executor`, `permissions`, `todo`, or `mcp`. `mcp` has its own [guide](mcp/README.md) and facade
for work inside that subtree. The shared tool surface itself is `finch_tools_api::Item` — `src/tools/mod.rs` and the
`types.rs`/`permissions.rs` re-export shims keep the crate-internal paths working, and those shims
carry zero `crate::` imports (the former knot metric).

**Dependencies:** the application-bound side (executor, implementations, MCP, todo, diagnostics)
may depend on any composition-root subsystem, and each of those may import `tools` back — that
reverse edge is why implementations stay here. The shared surface they implement
(`finch-tools-api`) depends on nothing in the root crate; add no new dependency from the API
crate to any `src/` subsystem, and add no new authority surface outside it. `code_outline` is a
thin adapter over the one-way `tools -> source_index` dependency: source identity, parsing,
provenance, and stale-result semantics remain owned by that capsule.

**Permissions are authority.** Peer and constitutional rules — defined in the
`finch-tools-api` crate, re-exported here — are invariants, not defaults to relax. Facade changes
must not alter allowlist behavior: `is_readonly_bash()` still rejects shell operators, peers still
cannot restart or spawn, and write/edit/patch still surface as AskUser. See the root [Security invariant](../../CLAUDE.md#security). The peer
hard-deny and allow tables are keyed on registered tool names (`PEER_HARD_DENY_TOOLS`,
`PEER_SILENT_ALLOW_TOOLS`, `PEER_REVIEWED_CHANGESET_TOOLS`, `VM_DISCOVERY_TOOLS`); conformance
tests in this subtree and in `src/cli/repl/always_allow_tests.rs` fail if a policy table names
anything no `Tool` registers or alias key covers.

**Workspace containment is authority (issue #429).** Path arguments are canonicalised
(symlinks and `..`) against the workspace root — git root when present, else cwd —
before Allow, peer silent-allow, or pattern match. Escape is one-shot AskUser; a
pattern can never satisfy it. `invocation_runs_autonomously` is the auto-approve
predicate so WorkspaceRead cannot skip the escape dialog. `PathSlot::WorkspaceContained`
means “any path under the workspace root”, not “any string”. Bash has no path slot.
`code_outline.path` is a discrete path slot and receives the same containment decision as
`read.file_path`; `ToolExecutor::new` rejects any path-bound tool whose declared execution root
differs from the permission root, and the implementation capability-opens beneath that same root.
`test_dotdot_escape_is_ask_user_not_allow`, `test_symlink_escape_is_ask_user_live_and_dangling`,
`test_escaped_path_is_not_pattern_admissible_through_approval_path`, and
`test_star_pattern_does_not_match_escaped_path` pin this.

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
pre-refactor classification. The planning allowlist (`PLANNING_ALLOWED_TOOLS` in `src/cli/repl_event/plan_handler.rs`, shared
by the dispatch path and the executor gate) is keyed on registered names or alias keys and is
conformance-tested in the same file; spellings nothing registers (`ExitPlanMode`, `Bash`) are
deliberately blocked. Declaring `Unclassified` is a real decision: enter_plan_mode, inspect_memory, and the four agent
tools preserve their pre-refactor approval behavior that way, and re-authorizing any of them is a
deliberate approval-policy change with its own review, not a drive-by declaration edit. Issue #426
re-authorized `todo_read` (`VmRead`), `todo_write` (`VmWrite`), `present_plan` (`VmWrite`), and
`ask_user_question` (`VmWrite`): a session-local checklist and tools that already present their
own dialogs must not demand a second host-effect confirmation.

**A tool name is not an instruction.** Implementations receive model-supplied names, paths, and
schemas as data. MCP names are namespaced before they reach the registry; do not invent a second
permission path around that.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- tools::` for the
composition-root side, and `./scripts/test_brains.sh cargo test --lib -p finch-tools-api` for the
API surface (the pure permission-policy tests moved there with the policy). The
implementation-boundary security tests remain at `src/tools/permissions/tests.rs`, at their
original `tools::permissions::tests::*` paths. Run the full suite when changing a re-exported
`pub` item or the permission policy, because the CLI, runtime, scheduler, and providers all hold
these types.
