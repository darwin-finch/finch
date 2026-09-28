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

**A check-command spawn retries a transient `ETXTBSY`, bounded, instead of surfacing it as a
startup failure (issue #1204).** The kernel refuses to exec a file any process holds open for
writing, and `fork()` copies the *entire* file descriptor table — so on a Linux-family kernel, a
spawn anywhere else in the same process that forks while any thread still holds a write
descriptor on a file this service just wrote hands its child an inherited copy of that
descriptor, and this service's own exec is refused even though its own writer already closed its
copy first. `cargo test`'s default parallelism runs many `#[tokio::test]` functions concurrently
in one process, and this module both writes fresh executable fixtures and spawns them, repeatedly,
across many tests, which is exactly the combination that behavior makes racy — three separate CI
failures (main run 36262031799, PRs #1144 and #1193), each in a different test in this module,
never one whose diff touched `diagnostics/`. `spawn_retrying_text_file_busy` in
`diagnostics/mod.rs` mirrors the identical, already-reviewed fix for `process-run`
(`finch_runtime::host::spawn_retrying_text_file_busy`, issue #287): retry specifically on
`ETXTBSY`, bounded (eight attempts, linearly backed off from 5ms), never anything else. Verified
directly (not merely asserted): a shebang script's own exec is *not* subject to this kernel
check on macOS, so the deterministic reproduction below is Linux-family-only, while the
production retry stays `cfg(unix)` as a no-cost safety net.
`test_transient_text_file_busy_check_command_is_retried_not_reported_as_a_failure` in
`diagnostics/mod.rs` forces a real refusal (a second write descriptor held open on the exact
script about to be exec'd — the kernel's check is per-inode, so this is deterministic, not
timing-dependent) and proves the retry recovers it; the concurrent-load counterpart
`test_many_concurrent_check_command_spawns_never_report_a_startup_failure` (`worker_threads = 8`,
unlike every other test in this module) stresses genuinely overlapping forks across real OS
threads and asserts every one of many concurrent spawns still resolves to a passing check.

**ToolLoop owns REPL, scheduler, and headless `finch agent` rounds (issue #1058).** All three
callers admit a call through the `finch-tools-api` protocol before execution; malformed
arguments, duplicate ids, unknown or unsupported tools fail closed with a typed result. Cancel,
timeout, disconnect, retry, and late-result-after-terminal must not admit another execution or
append another result. Generators and provider adapters must not import or invoke `ToolExecutor`
themselves.

**Audit result for the legacy headless `finch agent` loop (issue #1058, `AgentLoop::run_task` in
`src/agent/mod.rs`).** Before this issue, `run_task` called `ToolExecutor::execute_tool` directly
for each provider `ToolUse`, with no round-admission lifecycle: nothing stopped two `ToolUse`
blocks sharing one tool-call id from both executing. Traced against the same five hostile-provider
behaviors the REPL/scheduler `ToolLoop` integration defends:
- **Malformed arguments:** structurally cannot reach this loop as raw fragments — `run_task` uses
  `ClaudeClient::send_message` (non-streaming; see `src/claude/client.rs`), so every adapter (see
  `finch-providers/AGENTS.md`, "tool calls become semantic `ToolUse` only after adapter
  validation") already parses/validates JSON tool arguments before constructing
  `ContentBlock::ToolUse`, or the whole `send_message` call fails
  (`openai.rs`'s `"OpenAI returned malformed JSON function arguments"` /
  `"...were not a JSON object"` bails, `claude.rs`'s `response.json()` failing the same way). This
  is coarser-grained than `ToolLoop`'s per-call reject (a malformed adapter payload fails the whole
  turn, not just that call) but strictly more conservative: no call ever executes on malformed
  input either way. `ToolLoop::observe_complete` itself performs no `is_object` check on an
  already-complete call (only the delta-accumulation path does); this matches REPL/scheduler
  exactly, since a non-streaming provider always calls `observe_complete` directly, never
  `observe_delta`.
- **Duplicate tool-call ids — the one real gap found, now fixed.** `run_task` now builds one
  `ToolLoop` per turn (catalog = `tool_defs`, the exact registered set) and runs every `ToolUse`
  through `observe_complete` → `finish_observation` → `admit_execution` → `append_result` before
  calling `ToolExecutor::execute_tool`, identically to `src/scheduler.rs`'s own agent turn loop
  (~line 984). Two `ToolUse` blocks sharing an id with conflicting inputs now fail *both* closed
  (matching `test_tool_loop_duplicate_id_fails_closed_without_second_execution`) instead of
  double-executing. `test_headless_agent_duplicate_tool_call_id_is_not_double_executed` in
  `src/agent/mod.rs` reproduces the pre-fix defect through the real `run_task` and a real
  `BashTool`/`ToolExecutor` (a side-effect marker file went from two writes to zero) and pins the
  fix.
- **Unknown/unsupported tool names:** already fail closed pre-fix (the registry lookup in
  `ToolExecutor::execute_tool` returns a typed `Ok(ToolResult::error(...))`, never executing) and
  continue to via `ToolLoop`'s catalog check (`RejectReason::UnsupportedTool`/`UnknownTool`).
  `test_headless_agent_unknown_tool_name_never_executes_and_does_not_abort_the_turn` proves a
  never-registered tool name produces no side effect while a second, real call in the same turn
  still runs.
- **Cancellation mid-call:** `finch agent` (`src/main.rs`'s `run_agent_command`) never installs a
  `tokio::signal::ctrl_c()` handler or holds a cancellation token anywhere in this loop — unlike the
  REPL/scheduler, there is no live signal that can arrive *during* an in-flight `execute_tool`
  call. The only way to stop a running headless task is killing the process, which terminates
  every in-flight state uniformly; there is no partial-admission state to leak. `ToolLoop`'s
  `terminalize`/late-result guards exist for a concurrent cancel signal this loop structurally does
  not have, so this behavior class does not apply here and is not simulated.
- **Late/out-of-order results:** `run_task`'s tool dispatch is a plain sequential `for` loop —
  each `ToolExecutor::execute_tool(...).await` is fully awaited before the next call is even
  admitted, and results are appended to `result_blocks` in the same iteration. There is no
  concurrent execution or channel through which a result could arrive after the round moved on;
  the race class `ToolLoop`'s late-result-after-terminal guard defends against cannot occur here by
  construction, independent of the `ToolLoop` integration.

**Preserved through the fix:** the agent-mode permission rule
(`PermissionManager::with_default_rule(PermissionRule::Allow)` in `build_tool_executor`) is
untouched — `ToolLoop` only gates catalog/duplicate-id admission, never permission, and
`ToolExecutor::execute_tool` is still the sole execution call. Provider-visible result order is
preserved because `finish_observation()` returns calls in first-observed order (see
`finch-tools-api`'s own ordering tests) and `run_task` pushes into `result_blocks` by plain
iteration over that same order, with no reordering step.

**The Claude Code MCP bridge is a translator, not a caller of this subtree's execution authority
(issue #1309, corrected by issue #1341, `src/cli/claude_cli_bridge.rs`).** `finch_providers::ClaudeCliProvider`
spawns the real `claude` CLI with its own built-in tools disabled and instead re-invokes this same
Finch binary as an MCP server. That subprocess used to build its own `ToolRegistry` and
`PermissionManager::for_peer()` and call `tool.execute()` directly — narrower even than
`ToolExecutor::execute_tool`, whose `AskUser` branch assumes a coordinator already obtained
approval, and with no coordinator or interactive TUI to ask a real human. That was a second,
disconnected authority and has been removed. The bridge now keeps only a `ToolRegistry` for
`tools/list` schema advertisement and MCP wire-name resolution — never for execution — and
forwards every `tools/call` over a Unix domain socket to `finch_providers::ClaudeCliProvider`,
running in the frontend process that owns the Brain's turn. That transport surfaces the call as an
ordinary `StreamChunk::ToolCallComplete`, so it reaches this subtree's real `ToolLoop`/
`ToolExecutionCoordinator`/`ToolExecutor` exactly the way every other provider's tool calls do —
real approval, real file access, no second gate to re-derive. See `finch-providers/AGENTS.md`'s
`ClaudeCliProvider` entry for the parked-child mechanism that lets a real approval decision, which
can take arbitrarily long, answer the bridge's still-open connection without this subtree changing
at all.

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
crate to any `src/` subsystem, and add no new authority surface outside it. `code_outline` and
`find_code` form a one-way `tools -> source_index` dependency: source identity, parsing,
provenance, cache format, and stale-result semantics remain owned by that capsule. Lexical ranking and
the optional application-specific local disambiguation port stay here; they must not enter
`source_index` or depend on MemTree.

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
`find_code.path`, when supplied, is a workspace-contained permission slot and an in-memory scope on
the already capability-bounded index; omitting it searches only the workspace injected at
construction. Its `WorkspaceRead` authority includes publication of disposable, bounded derived
index bytes under the separately injected application-state capability; it cannot mutate source,
conversation, provider, or approval state. One-shot query mode fails closed and the REPL omits this
tool when no home-backed application-state root is available; neither falls back to a workspace
cache. Current roots bind no disambiguator, so ambiguity never
reconstructs or calls a provider. Index freshness/build work runs off the async worker thread and
cooperatively fails after 20 seconds or 256 MiB of aggregate source reads, including validation.
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
