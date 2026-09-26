//! Tests that exercise the real registered tool implementations against the
//! permission policy (peer hard-deny, declared effects, planning-mode
//! re-authorizations). The pure policy tests moved with the production code
//! into `crates/finch-tools-api/src/permissions.rs`.

use super::*;
use finch_tools_api::ExecutionEffect;
use std::sync::Arc;

/// Null provider — fails on any actual call; used to construct TaskTool
/// so the test can read the name the real tool implementation registers.
struct NullProvider;

#[async_trait::async_trait]
impl crate::providers::ProviderBackend for NullProvider {
    async fn send_message_validated(
        &self,
        _req: crate::providers::ValidatedProviderRequest,
    ) -> anyhow::Result<crate::providers::ProviderResponse> {
        anyhow::bail!("null provider")
    }
    async fn send_message_stream_validated(
        &self,
        _req: crate::providers::ValidatedProviderRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<crate::providers::StreamChunk>>>
    {
        anyhow::bail!("null provider")
    }
    fn name(&self) -> &str {
        "null"
    }
    fn default_model(&self) -> &str {
        "null"
    }
}

#[test]
fn test_peer_cannot_restart() {
    let mgr = PermissionManager::for_peer();
    // Exercise the name the real RestartTool registers, not a literal: a
    // deny arm keyed on a name no tool registers is unreachable in
    // production and would let a peer call fall through to AskUser.
    let name = crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool);
    let input = serde_json::json!({});
    let check = mgr.check_tool_use(name, &input);
    assert!(
        matches!(check, PermissionCheck::Deny(_)),
        "invariant: a peer must be hard-denied for '{name}', the tool name \
             RestartTool registers; got {check:?} (AskUser means the hard-deny \
             arm is unreachable for the real tool name)"
    );
}
#[test]
fn test_peer_cannot_spawn() {
    let mgr = PermissionManager::for_peer();
    // Exercise the name the real TaskTool registers, not a literal.
    let task_tool =
        crate::tools::implementations::spawn::TaskTool::new(std::sync::Arc::new(NullProvider));
    let name = crate::tools::Tool::name(&task_tool);
    let input = serde_json::json!({});
    let check = mgr.check_tool_use(name, &input);
    assert!(
        matches!(check, PermissionCheck::Deny(_)),
        "invariant: a peer must be hard-denied for '{name}', the tool name \
             TaskTool registers; got {check:?} (AskUser means the hard-deny \
             arm is unreachable for the real tool name)"
    );
}

#[test]
fn test_peer_hard_deny_table_names_are_declared_by_real_tool_implementations() {
    // Conformance against the same drift class as the original defect:
    // every name in PEER_HARD_DENY_TOOLS must be a name the real Tool
    // implementations register, and the table must name exactly the
    // restart/spawn tools — otherwise the deny arm has drifted onto an
    // unregistered literal (unreachable in production) or a registered name
    // has lost its deny.
    let task_tool =
        crate::tools::implementations::spawn::TaskTool::new(std::sync::Arc::new(NullProvider));
    let mut declared: Vec<String> = vec![
        crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool).to_string(),
        crate::tools::Tool::name(&task_tool).to_string(),
    ];
    declared.sort();
    let mut table: Vec<String> = PEER_HARD_DENY_TOOLS
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    table.sort();
    assert_eq!(
        table, declared,
        "PEER_HARD_DENY_TOOLS must name exactly the tool names the restart \
             and spawn Tool implementations register \
             (declared = {declared:?}, table = {table:?})"
    );
    // Cross-check the declared effects: a hard-denied tool must never
    // declare an autonomously-runnable effect, and the declarations must
    // reproduce exactly what the pre-#466 table computed for these
    // canonical names (restart_session → Destructive, spawn_task →
    // ExternalWrite), so the deny table and the effect declarations
    // cannot drift apart unnoticed.
    for (name, effect, expected) in [
        (
            crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool),
            crate::tools::Tool::effect(&crate::tools::implementations::restart::RestartTool),
            ExecutionEffect::Destructive,
        ),
        (
            crate::tools::Tool::name(&task_tool),
            crate::tools::Tool::effect(&task_tool),
            ExecutionEffect::ExternalWrite,
        ),
    ] {
        assert_eq!(
            effect, expected,
            "peer hard-deny tool '{name}' must declare {expected:?} — the \
                 classification the pre-refactor table computed for it"
        );
        assert!(
            matches!(
                effect,
                ExecutionEffect::Destructive | ExecutionEffect::ExternalWrite
            ),
            "peer hard-deny tool '{name}' declares {effect:?}; a hard-denied \
                 tool must declare Destructive or ExternalWrite so the deny and \
                 the declaration agree"
        );
        assert!(
            !effect.runs_autonomously(),
            "peer hard-deny tool '{name}' must not declare an autonomous effect"
        );
    }
}
#[test]
fn test_declared_effects_auto_run_reads_but_not_writes() {
    use crate::tools::Tool;
    assert_eq!(
        crate::tools::ReadTool.effect(),
        ExecutionEffect::WorkspaceRead
    );
    assert!(crate::tools::ReadTool.effect().runs_autonomously());
    assert!(!crate::tools::WriteTool.effect().runs_autonomously());
    assert!(
        !ExecutionEffect::Unclassified.runs_autonomously(),
        "Unclassified must never run autonomously"
    );
}
#[test]
fn typed_program_declaration_ignores_untrusted_coarse_effect_input() {
    use crate::tools::{SubmitProgramTool, Tool};
    let tool = SubmitProgramTool::new(Arc::new(crate::runtime::ProgramRuntime::new()));
    assert_eq!(tool.effect(), ExecutionEffect::VmWrite);
    // The declaration is a property of the tool; a model-supplied coarse
    // label in the input payload is no longer consulted anywhere.
    for effect in ["pure", "destructive", "invented"] {
        let input = serde_json::json!({"effect": effect});
        assert_eq!(
            refined_effect_for_approval(tool.effect(), "submit_program", &input),
            ExecutionEffect::VmWrite,
            "input payload {effect} must not change the declared effect"
        );
    }
}
#[test]
fn test_bash_readonly_refinement_applies_to_the_registered_bash_tool_name() {
    use crate::tools::{BashTool, Tool};
    // Pin the refinement's literal to the name the real tool registers so
    // a rename cannot strand the refinement (the #452/#466 drift class).
    assert_eq!(
        BashTool.name(),
        "bash",
        "refined_effect_for_approval refines the name BashTool registers; \
             if this fails, update the literal there together with this pin"
    );
    assert_eq!(
        refined_effect_for_approval(
            BashTool.effect(),
            BashTool.name(),
            &serde_json::json!({"command": "ls -la"})
        ),
        ExecutionEffect::WorkspaceRead,
        "read-only bash must refine the declared worst case to WorkspaceRead"
    );
    assert_eq!(
        refined_effect_for_approval(
            BashTool.effect(),
            BashTool.name(),
            &serde_json::json!({"command": "git commit -m x"})
        ),
        ExecutionEffect::ExternalWrite,
        "side-effecting bash keeps the declared worst case"
    );
}
#[test]
fn test_bash_readonly_refinement_still_rejects_shell_operators() {
    use crate::tools::{BashTool, Tool};
    // Prefix-bypass invariant: operators disqualify the refinement.
    for cmd in ["ls; rm file", "cat foo | tee out.txt", "echo hi > file"] {
        assert_eq!(
            refined_effect_for_approval(
                BashTool.effect(),
                BashTool.name(),
                &serde_json::json!({"command": cmd})
            ),
            ExecutionEffect::ExternalWrite,
            "'{cmd}' must not refine to WorkspaceRead"
        );
    }
}
#[test]
fn vm_discovery_tools_are_autonomous_vm_reads() {
    let memory_dir = tempfile::TempDir::new().expect("temp dir for memory catalog");
    let memory = Arc::new(
        finch_memory::MemorySystem::new(finch_memory::MemoryConfig {
            db_path: memory_dir.path().join("memory.db"),
            use_neural_embeddings: false,
            ..finch_memory::MemoryConfig::default()
        })
        .expect("memory system for vm discovery classification"),
    );
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());

    let mut registry = crate::tools::ToolRegistry::new();
    for tool in [
        Box::new(crate::tools::GetVmStateTool::new(Arc::clone(&runtime)))
            as Box<dyn crate::tools::Tool>,
        Box::new(crate::tools::GetLanguageDefinitionTool),
        Box::new(crate::tools::SearchVmVocabularyTool::new(Arc::clone(
            &runtime,
        ))),
        Box::new(crate::tools::InspectVmWordTool::new(Arc::clone(&runtime))),
        Box::new(crate::tools::SearchWordTool::new(
            Arc::clone(&runtime),
            None,
        )),
        Box::new(crate::tools::InspectWordTool::new(
            Arc::clone(&runtime),
            None,
        )),
        Box::new(crate::tools::SearchVocabularyTool::new(Arc::clone(&memory))),
        Box::new(crate::tools::InspectProgramTool::new(memory)),
    ] {
        registry.register(tool);
    }

    for tool in VM_DISCOVERY_TOOLS {
        assert_eq!(
            registry.declared_effect(tool),
            ExecutionEffect::VmRead,
            "{tool} must not open a host-effect approval dialog"
        );
        assert!(
            registry.declared_effect(tool).runs_autonomously(),
            "{tool} is VM discovery and must run autonomously"
        );
    }
}
#[test]
fn session_local_tools_skip_host_effect_confirmation_at_approval_boundary() {
    // Issue #426: spawn_tool_execution auto-approves when
    // refined_effect_for_approval(declared, name, input).runs_autonomously().
    // Unclassified never does, so the canonical names prompted. The
    // four-item todo_write payload is the reported failure.
    use crate::tools::{
        AskUserQuestionTool, PresentPlanTool, TodoReadTool, TodoWriteTool, ToolRegistry,
    };

    let todo_list = Arc::new(tokio::sync::RwLock::new(crate::tools::TodoList::default()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(TodoWriteTool::new(Arc::clone(&todo_list))));
    registry.register(Box::new(TodoReadTool::new(todo_list)));
    registry.register(Box::new(PresentPlanTool));
    registry.register(Box::new(AskUserQuestionTool));
    registry.register(Box::new(crate::tools::WriteTool));
    registry.register_alias("TodoWrite", "todo_write");
    registry.register_alias("TodoRead", "todo_read");
    registry.register_alias("PresentPlan", "present_plan");
    registry.register_alias("AskUserQuestion", "ask_user_question");

    let four_item_list = serde_json::json!({"todos": [
        {"content": "Identify the harness implementation, website source, and deployment target",
         "id": "1", "priority": "high", "status": "in_progress"},
        {"content": "Implement the typed program runner in the harness",
         "id": "2", "priority": "high", "status": "pending"},
        {"content": "Build the website source from the harness output",
         "id": "3", "priority": "medium", "status": "pending"},
        {"content": "Verify the deployment target accepts the build",
         "id": "4", "priority": "low", "status": "completed"}
    ]});

    for (name, expected, input) in [
        ("todo_write", ExecutionEffect::VmWrite, &four_item_list),
        ("TodoWrite", ExecutionEffect::VmWrite, &four_item_list),
        ("todo_read", ExecutionEffect::VmRead, &serde_json::json!({})),
        (
            "present_plan",
            ExecutionEffect::VmWrite,
            &serde_json::json!({"plan": "do the work"}),
        ),
        (
            "ask_user_question",
            ExecutionEffect::VmWrite,
            &serde_json::json!({"questions": []}),
        ),
    ] {
        let declared = registry.declared_effect(name);
        let refined = refined_effect_for_approval(declared, name, input);
        assert_eq!(
            declared, expected,
            "invariant: '{name}' must declare {expected:?} at the approval \
                 boundary; Unclassified is AskUser in spawn_tool_execution \
                 (declared={declared:?}, input={input})"
        );
        assert_ne!(
            refined,
            ExecutionEffect::Unclassified,
            "invariant: '{name}' must not present Unclassified to \
                 refined_effect_for_approval; that is the AskUser the user hits \
                 (declared={declared:?}, refined={refined:?}, input={input})"
        );
        assert!(
            refined.runs_autonomously(),
            "invariant: '{name}' must skip host-effect confirmation; \
                 spawn_tool_execution emits ToolApprovalNeeded when the refined \
                 effect does not run autonomously (declared={declared:?}, \
                 refined={refined:?}, input={input})"
        );
    }

    let write_input = serde_json::json!({"file_path": "src/lib.rs", "content": "x"});
    let write_refined =
        refined_effect_for_approval(registry.declared_effect("write"), "write", &write_input);
    assert!(
        !write_refined.runs_autonomously(),
        "control: workspace write must still demand host-effect confirmation; \
             refined={write_refined:?}"
    );

    let owner = PermissionManager::new();
    let peer = PermissionManager::for_peer();
    for name in [
        "todo_write",
        "todo_read",
        "present_plan",
        "ask_user_question",
    ] {
        assert!(
            !matches!(
                owner.check_tool_use(name, &serde_json::json!({})),
                PermissionCheck::Deny(_)
            ),
            "invariant: PermissionManager must not Deny '{name}'; \
                 constitutional constraints still apply to bash/read/web_fetch \
                 only (got {:?})",
            owner.check_tool_use(name, &serde_json::json!({}))
        );
        assert!(
            !matches!(
                peer.check_tool_use(name, &serde_json::json!({})),
                PermissionCheck::Deny(_)
            ),
            "invariant: '{name}' is not a peer hard-deny tool; got {:?}",
            peer.check_tool_use(name, &serde_json::json!({}))
        );
    }

    let restart = crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool);
    assert!(
        matches!(
            peer.check_tool_use(restart, &serde_json::json!({})),
            PermissionCheck::Deny(_)
        ),
        "invariant: peer hard-deny still blocks '{restart}'"
    );
    assert!(
        matches!(
            owner.check_tool_use("bash", &serde_json::json!({"command": "rm -rf /"})),
            PermissionCheck::Deny(_)
        ),
        "invariant: constitutional constraints still deny dangerous bash"
    );
}
