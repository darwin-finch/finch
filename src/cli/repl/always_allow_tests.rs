use super::{apply_repl_always_allow_tools, register_repl_tool_aliases, REPL_ALWAYS_ALLOW_TOOLS};
use crate::config::Config;
use crate::generators::{Generator, GeneratorCapabilities, GeneratorResponse};
use crate::runtime::ProgramRuntime;
use crate::scheduler::{AgentScheduler, ProviderResolver};
use crate::tools::{
    invocation_runs_autonomously, refined_effect_for_approval, AgentAwaitTool, AgentCancelTool,
    AgentPollTool, AgentSpawnTool, AnsibleTool, AskUserQuestionTool, BackgroundBashTool,
    BackgroundPollTool, BackgroundStopTool, BashTool, ClaudeCodeDelegateTool, CodeOutlineTool,
    CreateMemoryTool, EditTool, EnterPlanModeTool, FindCodeTool, GetLanguageDefinitionTool,
    GetVmStateTool, GlobTool, GrepTool, HashCompareTool, InspectMemoryTool, InspectWordTool,
    ListRecentTool, PatchTool, PermissionCheck, PermissionManager, PermissionRule, PresentPlanTool,
    ReadTool, RestartTool, SearchMemoryTool, SearchWordTool, SubmitProgramTool, TaskTool,
    TodoReadTool, TodoWriteTool, Tool, ToolRegistry, WebFetchTool, WriteTool,
};
use finch_programs::ExecutionEffect;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn find_code_state_requires_an_explicit_home_root() {
    assert_eq!(
        super::source_index_state_for_home(Some(std::path::Path::new("/home/example"))),
        Some(PathBuf::from("/home/example/.finch/source-index"))
    );
    assert_eq!(super::source_index_state_for_home(None), None);
}
use std::sync::Arc;

struct NameAuditGenerator;

#[async_trait::async_trait]
impl Generator for NameAuditGenerator {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<GeneratorResponse> {
        anyhow::bail!("name-audit generator is not invoked")
    }

    async fn generate_stream(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<
        Option<tokio::sync::mpsc::Receiver<anyhow::Result<crate::generators::StreamChunk>>>,
    > {
        Ok(None)
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        static CAPABILITIES: GeneratorCapabilities = GeneratorCapabilities {
            supports_streaming: false,
            supports_tools: true,
            supports_conversation: true,
            max_context_messages: Some(1),
        };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "name-audit"
    }
}

/// Fails on any call; TaskTool's default provider is never invoked by these
/// name/effect/allowlist conformance tests.
struct NameAuditProvider;

#[async_trait::async_trait]
impl crate::providers::ProviderBackend for NameAuditProvider {
    async fn send_message_validated(
        &self,
        _req: crate::providers::ValidatedProviderRequest,
    ) -> anyhow::Result<crate::providers::ProviderResponse> {
        anyhow::bail!("name-audit provider is not invoked")
    }
    async fn send_message_stream_validated(
        &self,
        _req: crate::providers::ValidatedProviderRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<crate::providers::StreamChunk>>>
    {
        anyhow::bail!("name-audit provider is not invoked")
    }
    fn name(&self) -> &str {
        "name-audit"
    }
    fn default_model(&self) -> &str {
        "name-audit"
    }
}

struct OwnerReplCatalog {
    registry: ToolRegistry,
    _memory_dir: tempfile::TempDir,
}

fn owner_repl_catalog() -> OwnerReplCatalog {
    let memory_dir = tempfile::TempDir::new().expect("temp dir for memory catalog");
    let memory = Arc::new(
        finch_memory::MemorySystem::new(finch_memory::MemoryConfig {
            db_path: memory_dir.path().join("memory.db"),
            use_neural_embeddings: false,
            ..finch_memory::MemoryConfig::default()
        })
        .expect("memory system for name catalog"),
    );
    let runtime = Arc::new(ProgramRuntime::new());
    let scheduler = AgentScheduler::new(
        ProviderResolver::new(Arc::new(NameAuditGenerator)),
        Arc::clone(&runtime),
    );
    let todo_list = Arc::new(tokio::sync::RwLock::new(crate::tools::TodoList::default()));

    let mut registry = ToolRegistry::new();
    let background_tasks = std::sync::Arc::new(crate::brain::BackgroundTaskManager::new());
    for tool in [
        Box::new(ReadTool) as Box<dyn Tool>,
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(CodeOutlineTool::new(std::env::current_dir().expect("cwd"))),
        Box::new(FindCodeTool::new(
            std::env::current_dir().expect("cwd"),
            memory_dir.path().join("source-index"),
        )),
        Box::new(WebFetchTool::new()),
        Box::new(BashTool),
        Box::new(BackgroundBashTool::new(std::sync::Arc::clone(
            &background_tasks,
        ))),
        Box::new(BackgroundPollTool::new(std::sync::Arc::clone(
            &background_tasks,
        ))),
        Box::new(BackgroundStopTool::new(background_tasks)),
        Box::new(ClaudeCodeDelegateTool::new(
            std::env::current_dir().expect("cwd"),
        )),
        Box::new(TaskTool::new(
            Arc::new(NameAuditProvider),
            Arc::new(Config::new(vec![])),
        )),
        Box::new(EditTool),
        Box::new(PatchTool),
        Box::new(WriteTool),
        Box::new(HashCompareTool),
        Box::new(AnsibleTool),
        Box::new(RestartTool),
        Box::new(EnterPlanModeTool),
        Box::new(PresentPlanTool),
        Box::new(AskUserQuestionTool),
        Box::new(GetLanguageDefinitionTool),
        Box::new(SubmitProgramTool::new(Arc::clone(&runtime))),
        Box::new(GetVmStateTool::new(Arc::clone(&runtime))),
        Box::new(SearchWordTool::new(
            Arc::clone(&runtime),
            Some(Arc::clone(&memory)),
        )),
        Box::new(InspectWordTool::new(
            Arc::clone(&runtime),
            Some(Arc::clone(&memory)),
        )),
        Box::new(SearchMemoryTool::new(Arc::clone(&memory))),
        Box::new(InspectMemoryTool::new(Arc::clone(&memory))),
        Box::new(CreateMemoryTool::new(Arc::clone(&memory))),
        Box::new(ListRecentTool::new(memory)),
        Box::new(TodoWriteTool::new(Arc::clone(&todo_list))),
        Box::new(TodoReadTool::new(todo_list)),
        Box::new(AgentSpawnTool::new(Arc::clone(&scheduler))),
        Box::new(AgentAwaitTool::new(Arc::clone(&scheduler))),
        Box::new(AgentPollTool::new(Arc::clone(&scheduler))),
        Box::new(AgentCancelTool::new(scheduler)),
    ] {
        registry.register(tool);
    }
    register_repl_tool_aliases(&mut registry);
    OwnerReplCatalog {
        registry,
        _memory_dir: memory_dir,
    }
}

fn owner_repl_permissions() -> PermissionManager {
    let mut permissions = PermissionManager::new().with_default_rule(PermissionRule::Ask);
    apply_repl_always_allow_tools(&mut permissions);
    permissions
}

#[test]
fn test_always_allow_list_entries_are_registered_tools_or_aliases() {
    let catalog = owner_repl_catalog();
    let unknown: Vec<&str> = REPL_ALWAYS_ALLOW_TOOLS
        .iter()
        .copied()
        .filter(|name| !catalog.registry.has_tool(name))
        .collect();
    assert!(
        unknown.is_empty(),
        "always-allow list grants approval to names nothing registers: {unknown:?}. \
         Each entry must be a Tool::name() or a register_alias key from the owner REPL catalog."
    );
}

#[test]
fn test_peer_permission_policy_tables_name_only_registered_tools_or_aliases() {
    // The peer hard-deny in permissions.rs once matched "restart"/"spawn" —
    // names no Tool::name() or register_alias key produces — so the deny arm
    // was unreachable in production while the invariant tests passed on their
    // own literals. This test pins every permission policy table whose names
    // the REPL catalog must register, so a third drift cannot land silently.
    // PEER_HARD_DENY_TOOLS is pinned against the Tool implementations
    // themselves in tools::permissions::tests, which is also where
    // `TaskTool`'s provider-selection behavior (the optional `provider`
    // parameter, issue: subagent provider selection) is exercised;
    // `spawn_task` is registered here too (mirroring its real REPL
    // registration) so the declared-effect and allowlist conformance tests
    // below cover it exactly like any other owner-session tool. Nested
    // `spawn_task` calls inside a running subagent still build their own
    // tool lists in `build_subagent_tools`, not through this registry.
    let catalog = owner_repl_catalog();
    let registry = &catalog.registry;
    let tables = [
        (
            "PEER_SILENT_ALLOW_TOOLS",
            crate::tools::PEER_SILENT_ALLOW_TOOLS,
        ),
        (
            "PEER_REVIEWED_CHANGESET_TOOLS",
            crate::tools::PEER_REVIEWED_CHANGESET_TOOLS,
        ),
        ("VM_DISCOVERY_TOOLS", crate::tools::VM_DISCOVERY_TOOLS),
        (
            "PLANNING_ALLOWED_TOOLS",
            crate::cli::repl_event::PLANNING_ALLOWED_TOOLS,
        ),
    ];
    let mut unregistered: Vec<String> = Vec::new();
    for (table, names) in tables {
        for name in names {
            if !registry.has_tool(name) {
                unregistered.push(format!("{table} names '{name}'"));
            }
        }
    }
    // Single-name special cases in check_tool_use / check_peer_tool_use and
    // the approval-site bash refinement.
    for name in ["submit_program", "bash"] {
        if !registry.has_tool(name) {
            unregistered.push(format!("peer/owner special case names '{name}'"));
        }
    }
    assert!(
        unregistered.is_empty(),
        "permission policy tables grant or deny names nothing registers \
         (same drift class as the peer hard-deny in issue #452 and \
         legacy_tool_effect in issue #26): {unregistered:?}. \
         Registered names: {:?}; alias keys: {:?}. \
         Each policy entry must be a Tool::name() or a register_alias key.",
        registry.tool_names(),
        registry.alias_names()
    );
}

/// Pre-#466 classification oracle: what the deleted string-keyed
/// `legacy_tool_effect` table computed for each **canonical registered name**
/// before the refactor. The declared `Tool::effect` values must reproduce
/// these exactly (except bash, below), so the refactor is provably
/// behaviour-preserving at the approval boundary. The stale spellings the old
/// table carried ("todowrite", "enterplanmode", …) matched no registered name
/// and have no arm here — canonical names fell through to Unclassified, which
/// is what the approval boundary actually observed.
fn pre_refactor_effect(tool_name: &str) -> ExecutionEffect {
    match tool_name {
        "submit_program" => ExecutionEffect::VmWrite,
        "get_vm_state"
        | "get_language_definition"
        | "search_vm_vocabulary"
        | "inspect_vm_word"
        | "search_word"
        | "inspect_word"
        | "search_vocabulary"
        | "inspect_program"
        | "search_memory"
        | "list_recent_memories"
        | "todoread" => ExecutionEffect::VmRead,
        "todowrite" | "enterplanmode" | "presentplan" | "askuserquestion" | "create_memory" => {
            ExecutionEffect::VmWrite
        }
        "read" | "glob" | "grep" | "hash_compare" | "excel_read" | "excel_range"
        | "excel_sheets" | "gui_inspect" => ExecutionEffect::WorkspaceRead,
        "web_fetch" => ExecutionEffect::ExternalRead,
        "write" | "edit" | "patch" | "excel_write" | "excel_formula" => {
            ExecutionEffect::WorkspaceWrite
        }
        // The old table was input-dependent here: read-only commands yielded
        // WorkspaceRead, everything else ExternalWrite. The declaration is the
        // worst case; the read-only refinement is now explicit at the approval
        // sites (refined_effect_for_approval) and pinned in
        // tools::permissions::tests.
        "bash" => ExecutionEffect::ExternalWrite,
        "restart_session" => ExecutionEffect::Destructive,
        "run" | "spawn_task" | "ansible" | "gui_click" | "gui_type" | "excel_activate" => {
            ExecutionEffect::ExternalWrite
        }
        _ => ExecutionEffect::Unclassified,
    }
}

/// Declared effect the owner catalog must pin. Matches the pre-#466 table
/// except for the issue #426 re-authorization: canonical `todo_read`,
/// `todo_write`, `present_plan`, and `ask_user_question` no longer stay
/// Unclassified just because the legacy arms used stale spellings.
fn pinned_declared_effect(tool_name: &str) -> ExecutionEffect {
    match tool_name {
        "code_outline" | "find_code" => ExecutionEffect::WorkspaceRead,
        "todo_read" => ExecutionEffect::VmRead,
        "todo_write" | "present_plan" | "ask_user_question" => ExecutionEffect::VmWrite,
        // Issue #754: the background bash sibling carries bash's worst-case
        // authority, stop kills the recorded task process (the same envelope),
        // and polling only reads this session's captured output.
        "background_bash" | "background_stop" => ExecutionEffect::ExternalWrite,
        "background_poll" => ExecutionEffect::ExternalRead,
        // Delegating to the Claude Code CLI grants a second, independently-
        // authenticated agent the same real host authority bash has once it
        // starts (file writes, shell commands) — the same worst case
        // spawn_task already carries for an equivalent delegation.
        "delegate_to_claude_code" => ExecutionEffect::ExternalWrite,
        _ => pre_refactor_effect(tool_name),
    }
}

#[test]
fn test_declared_effects_match_pre_refactor_classification() {
    // Issue #466 acceptance, updated by #426: enumerate every registered tool
    // and assert its declared effect. A mismatch here means authority drifted
    // silently. The #426 tools are pinned to the NEW intended classification
    // (VmRead/VmWrite), not the Unclassified fall-through the canonical names
    // had under the stale-spelling table.
    let catalog = owner_repl_catalog();
    let mut checked = 0usize;
    for tool in catalog.registry.get_all_tools() {
        let name = tool.name();
        if name == "bash" {
            assert_eq!(
                tool.effect(),
                ExecutionEffect::ExternalWrite,
                "bash must declare its worst case; the read-only refinement \
                 lives at the approval sites, not in the declaration"
            );
        } else {
            assert_eq!(
                tool.effect(),
                pinned_declared_effect(name),
                "'{name}' declared {effect:?} but the pinned classification \
                 is {expected:?}; re-assert the intended classification or \
                 treat this as a deliberate approval-policy change",
                effect = tool.effect(),
                expected = pinned_declared_effect(name),
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 25,
        "the owner catalog must enumerate the real tool surface; only \
         {checked} tools were checked"
    );
}

#[test]
fn test_declared_effect_is_independent_of_dispatch_spelling() {
    // The deleted legacy table classified by the raw provider spelling:
    // "TodoWrite" (alias) yielded VmWrite and ran autonomously while the
    // canonical "todo_write" yielded Unclassified and prompted. Issue #426
    // re-authorized the canonical name to VmWrite as well. Declared effects
    // travel with the tool, so every dispatch spelling of a tool must
    // present the same authority.
    let catalog = owner_repl_catalog();
    assert!(
        !catalog.registry.alias_names().is_empty(),
        "the owner catalog must register aliases for this test to mean anything"
    );
    for alias in catalog.registry.alias_names() {
        let canonical = catalog
            .registry
            .get(&alias)
            .unwrap_or_else(|| panic!("alias {alias} must resolve to a registered tool"))
            .name()
            .to_string();
        assert_eq!(
            catalog.registry.declared_effect(&alias),
            catalog.registry.declared_effect(&canonical),
            "alias '{alias}' and canonical '{canonical}' must present the \
             same declared effect; classification must not depend on spelling"
        );
    }
}

/// The production approval predicate in
/// `ToolExecutionCoordinator::spawn_tool_execution`,
/// `spawn_changeset_batch`, and the sync REPL path: a tool prompts only
/// when the invocation does not run autonomously and there is no cached
/// approval. Escape is never autonomous.
fn spawn_site_runs_autonomously(
    registry: &ToolRegistry,
    name: &str,
    input: &serde_json::Value,
) -> bool {
    invocation_runs_autonomously(
        registry.declared_effect(name),
        name,
        input,
        &PermissionManager::new(),
    )
}

#[test]
fn test_session_local_tools_do_not_hit_unclassified_approval() {
    // Issue #426: canonical todo_write / todo_read / present_plan /
    // ask_user_question declared Unclassified after #466 preserved the
    // stale-spelling fall-through. Unclassified is AskUser at
    // spawn_tool_execution, so a four-item checklist waited on a host-effect
    // confirmation the user never expected. present_plan and ask_user_question
    // already have their own dialogs upstream; a second PermissionManager
    // AskUser is the reported double prompt.
    let catalog = owner_repl_catalog();
    let four_item_list = json!({"todos": [
        {"content": "Identify the harness implementation, website source, and deployment target",
         "id": "1", "priority": "high", "status": "in_progress"},
        {"content": "Implement the typed program runner in the harness",
         "id": "2", "priority": "high", "status": "pending"},
        {"content": "Build the website source from the harness output",
         "id": "3", "priority": "medium", "status": "pending"},
        {"content": "Verify the deployment target accepts the build",
         "id": "4", "priority": "low", "status": "completed"}
    ]});
    let plan_input = json!({"plan": "1. explore\n2. change files\n3. test"});
    let question_input = json!({"questions": [{
        "question": "Which approach?",
        "header": "Approach",
        "options": [
            {"label": "A", "description": "Fast"},
            {"label": "B", "description": "Simple"}
        ]
    }]});

    for (name, expected, input) in [
        ("todo_write", ExecutionEffect::VmWrite, &four_item_list),
        ("TodoWrite", ExecutionEffect::VmWrite, &four_item_list),
        ("todo_read", ExecutionEffect::VmRead, &json!({})),
        ("TodoRead", ExecutionEffect::VmRead, &json!({})),
        ("present_plan", ExecutionEffect::VmWrite, &plan_input),
        ("PresentPlan", ExecutionEffect::VmWrite, &plan_input),
        (
            "ask_user_question",
            ExecutionEffect::VmWrite,
            &question_input,
        ),
        ("AskUserQuestion", ExecutionEffect::VmWrite, &question_input),
    ] {
        let declared = catalog.registry.declared_effect(name);
        let refined = refined_effect_for_approval(declared, name, input);
        assert_eq!(
            declared, expected,
            "invariant: '{name}' must declare {expected:?} so a session-local \
             checklist or an already-dialogued tool does not demand host-effect \
             confirmation; declared={declared:?}"
        );
        assert_ne!(
            refined,
            ExecutionEffect::Unclassified,
            "invariant: '{name}' must not present Unclassified at the approval \
             boundary; Unclassified is AskUser in spawn_tool_execution \
             (declared={declared:?}, refined={refined:?}, input={input})"
        );
        assert!(
            spawn_site_runs_autonomously(&catalog.registry, name, input),
            "invariant: '{name}' must skip the host-effect confirmation at \
             spawn_tool_execution; Unclassified would emit ToolApprovalNeeded. \
             declared={declared:?} refined={refined:?} input={input}"
        );
    }

    let write_input = json!({"file_path": "src/lib.rs", "content": "x"});
    assert!(
        !spawn_site_runs_autonomously(&catalog.registry, "write", &write_input),
        "control: workspace write must still demand host-effect confirmation; \
         got declared={:?} refined={:?}",
        catalog.registry.declared_effect("write"),
        refined_effect_for_approval(
            catalog.registry.declared_effect("write"),
            "write",
            &write_input
        )
    );

    let escaped_read = json!({"file_path": "/etc/../etc/passwd"});
    assert!(
        !spawn_site_runs_autonomously(&catalog.registry, "read", &escaped_read),
        "invariant: an escaped WorkspaceRead must not auto-approve at \
         spawn_tool_execution; execute_tool treats AskUser as already confirmed"
    );
}

#[test]
fn test_planning_allowlists_only_admit_justified_tools() {
    // Justifies each planning-table entry against the declared effects. An
    // entry is admissible exactly when one of these holds:
    //   1. its declared effect is autonomous (reads, VM-local writes) —
    //      todo_read, todo_write, present_plan, and ask_user_question land
    //      here after the #426 re-authorization,
    //   2. it is bash, whose read-only refinement at the approval sites keeps
    //      read-only commands autonomous, or
    //   3. it re-enters planning mode (enter_plan_mode) — an idempotent
    //      session-local no-op while already planning.
    // A state-changing tool slipping onto a planning table fails here even
    // though its name registers fine. Names are resolved through the registry
    // first, so alias spellings are justified by their canonical tool.
    let catalog = owner_repl_catalog();
    const JUSTIFIED_EXCEPTIONS: &[&str] = &["bash", "enter_plan_mode"];
    for (table, names) in [(
        "PLANNING_ALLOWED_TOOLS",
        crate::cli::repl_event::PLANNING_ALLOWED_TOOLS,
    )] {
        for name in names {
            let tool = catalog
                .registry
                .get(name)
                .unwrap_or_else(|| panic!("{table} names '{name}' which must register"));
            let effect = tool.effect();
            let canonical = tool.name();
            let justified = effect.runs_autonomously() || JUSTIFIED_EXCEPTIONS.contains(&canonical);
            assert!(
                justified,
                "{table} admits '{name}' (canonical '{canonical}', declared \
                 effect {effect:?}) with no planning-mode justification; \
                 planning mode must not admit state-changing tools"
            );
        }
    }
}

#[test]
fn test_repl_planning_gate_blocks_spellings_nothing_registers() {
    // Issue #466: planning gates once allow-listed spellings no Tool
    // registered. "ExitPlanMode" is refused up front. "Bash" is NOT in that
    // class any more: #765 registered it as a dispatch alias of `bash`
    // because legacy compacted history carries it, so the alias-resolving
    // gate admits it — asserted in test_plan_mode_allows_enter_plan_mode_
    // spellings' siblings, not here.
    use crate::cli::ReplMode;
    let mode = ReplMode::Planning {
        task: String::new(),
        plan_path: std::path::PathBuf::from("/tmp/plan.md"),
        created_at: chrono::Utc::now(),
    };
    for tool in ["ExitPlanMode"] {
        assert!(
            !crate::cli::repl_event::is_tool_allowed_in_mode(tool, &mode),
            "{tool} registers as nothing and must not pass the planning gate"
        );
    }
    assert!(
        crate::cli::repl_event::is_tool_allowed_in_mode("Bash", &mode),
        "Bash is a registered dispatch alias of bash (#765 legacy replay) and \
         must pass the alias-resolving gate"
    );
    for tool in ["bash", "enter_plan_mode", "EnterPlanMode", "read"] {
        assert!(
            crate::cli::repl_event::is_tool_allowed_in_mode(tool, &mode),
            "{tool} must still pass the authoritative planning gate"
        );
    }
}

#[test]
fn test_always_allow_list_pre_approves_named_reads_and_agent_control() {
    for tool in [
        "read",
        "glob",
        "grep",
        "code_outline",
        "find_code",
        "web_fetch",
        "search_memory",
        "inspect_memory",
        "list_recent_memories",
        "get_vm_state",
        "get_language_definition",
        "search_word",
        "inspect_word",
        "search_vm_vocabulary",
        "inspect_vm_word",
        "search_vocabulary",
        "inspect_program",
        "spawn_agent",
        "await_agent",
        "poll_agent",
        "cancel_agent",
    ] {
        assert!(
            REPL_ALWAYS_ALLOW_TOOLS.contains(&tool),
            "{tool} must stay on the always-allow list; deleting it reintroduces an approval prompt"
        );
    }
}

#[test]
fn test_always_allow_list_does_not_grant_unregistered_names() {
    for tool in [
        "push",
        "stack_push",
        "stack_run",
        "stack_clear",
        "memory_read",
        "memory_list",
        "describe",
        "view",
        "search",
        "stack_depth",
        "stack_top",
    ] {
        assert!(
            !REPL_ALWAYS_ALLOW_TOOLS.contains(&tool),
            "{tool} registers as nothing and must not be pre-approved"
        );
    }
}

#[test]
fn test_always_allow_list_does_not_grant_writes() {
    for tool in [
        "write",
        "edit",
        "patch",
        "bash",
        "create_memory",
        "todo_write",
        "restart_session",
        "submit_program",
    ] {
        assert!(
            !REPL_ALWAYS_ALLOW_TOOLS.contains(&tool),
            "{tool} is a write or host-effect tool and must not be pre-approved \
             by the always-allow name list; authority is the declared effect"
        );
    }
}

#[test]
fn test_owner_repl_pre_approves_memory_reads_without_prompt() {
    let permissions = owner_repl_permissions();
    for tool in ["search_memory", "inspect_memory", "list_recent_memories"] {
        assert!(
            matches!(
                permissions.check_tool_use(tool, &json!({"query": "x", "memory_id": "y"})),
                PermissionCheck::Allow
            ),
            "{tool} is a memory read and must skip the owner approval prompt; \
             got {:?}",
            permissions.check_tool_use(tool, &json!({"query": "x", "memory_id": "y"}))
        );
    }
}

#[test]
fn test_owner_repl_does_not_pre_approve_unregistered_or_write_names() {
    let permissions = owner_repl_permissions();
    for tool in [
        "push",
        "stack_push",
        "stack_run",
        "stack_clear",
        "memory_read",
        "memory_list",
        "describe",
        "view",
        "search",
        "create_memory",
        "write",
    ] {
        let check = permissions.check_tool_use(tool, &json!({}));
        assert!(
            matches!(check, PermissionCheck::AskUser(_)),
            "{tool} must not be pre-approved; got {check:?}"
        );
    }
}
