use super::{apply_repl_always_allow_tools, register_repl_tool_aliases, REPL_ALWAYS_ALLOW_TOOLS};
use crate::generators::{Generator, GeneratorCapabilities, GeneratorResponse};
use crate::runtime::ProgramRuntime;
use crate::scheduler::{AgentScheduler, ProviderResolver};
use crate::tools::{
    AgentAwaitTool, AgentCancelTool, AgentPollTool, AgentSpawnTool, AnsibleTool,
    AskUserQuestionTool, BashTool, CreateMemoryTool, EditTool, EnterPlanModeTool,
    GetLanguageDefinitionTool, GetVmStateTool, GlobTool, GrepTool, HashCompareTool,
    InspectMemoryTool, InspectWordTool, ListRecentTool, PatchTool, PermissionCheck,
    PermissionManager, PermissionRule, PresentPlanTool, ReadTool, RestartTool, SearchMemoryTool,
    SearchWordTool, SubmitProgramTool, TodoReadTool, TodoWriteTool, Tool, ToolRegistry,
    WebFetchTool, WriteTool,
};
use serde_json::json;
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

struct OwnerReplCatalog {
    registry: ToolRegistry,
    _memory_dir: tempfile::TempDir,
}

fn owner_repl_catalog() -> OwnerReplCatalog {
    let memory_dir = tempfile::TempDir::new().expect("temp dir for memory catalog");
    let memory = Arc::new(
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: memory_dir.path().join("memory.db"),
            use_neural_embeddings: false,
            ..crate::memory::MemoryConfig::default()
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
    for tool in [
        Box::new(ReadTool) as Box<dyn Tool>,
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(WebFetchTool::new()),
        Box::new(BashTool),
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
fn test_always_allow_list_pre_approves_named_reads_and_agent_control() {
    for tool in [
        "read",
        "glob",
        "grep",
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
            "{tool} has side effects and must not skip the owner approval prompt"
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
