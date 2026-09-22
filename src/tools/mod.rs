// Tool execution system for local tool use
//
// Enables Finch to execute tools (WebFetch, Bash, Read, etc.) locally
// instead of only generating text responses.

mod diagnostics;
mod executor;
mod implementations;
mod mcp;
mod permissions;
mod todo;
mod types;

pub use crate::brain::{
    BrainTask as TodoItem, BrainTaskPriority as TodoPriority, BrainTaskStatus as TodoStatus,
};
pub use executor::{generate_tool_signature, ApprovalSource, ToolExecutor};
pub use finch_tools_api::{
    compile_policy_from_registry, semantic_tools_for_advertisement, tool_authority_from_effect,
    AdmitError, ExactApproval, MatchType, ObserveOutcome, PathSlot, PatternType,
    PersistentPatternStore, PreparedCall, RejectReason, RejectedCall, Tool, ToolCatalog, ToolLoop,
    ToolLoopIdentity, ToolLoopResult, ToolLoopTerminal, ToolPattern, ToolRegistry, ToolSignature,
    ValidatedCall,
};
pub use implementations::llm_tools::create_llm_tools;
pub use implementations::propose::{propose_artifact_with_decision, ProposalDecision};
pub use implementations::restart::DeferredFrontendRestart;
pub use implementations::{
    AgentAwaitTool, AgentCancelTool, AgentPollTool, AgentSpawnTool, AnsibleTool,
    AskUserQuestionTool, BackgroundBashTool, BackgroundPollTool, BackgroundStopTool, BashTool,
    CreateMemoryTool, EditTool, EnterPlanModeTool, GetLanguageDefinitionTool, GetVmStateTool,
    GlobTool, GrepTool, HashCompareTool, InspectMemoryTool, InspectProgramTool, InspectVmWordTool,
    InspectWordTool, LLMDelegationTool, ListRecentTool, PatchTool, PresentPlanTool, ReadTool,
    RestartTool, SearchMemoryTool, SearchVmVocabularyTool, SearchVocabularyTool, SearchWordTool,
    SubmitProgramTool, TodoReadTool, TodoWriteTool, WebFetchTool, WriteTool,
};
#[cfg(target_os = "macos")]
pub use implementations::{
    ExcelActivateTool, ExcelFormulaTool, ExcelRangeTool, ExcelReadTool, ExcelSheetsTool,
    ExcelWriteTool, GuiClickTool, GuiInspectTool, GuiTypeTool,
};
pub use mcp::{McpClient, McpConnection, McpServerConfig, McpToolDescriptor, TransportType};
pub use permissions::{
    invocation_runs_autonomously, refined_effect_for_approval, PermissionCheck, PermissionManager,
    PermissionRule, ToolPermissionConfig, PEER_HARD_DENY_TOOLS, PEER_REVIEWED_CHANGESET_TOOLS,
    PEER_SILENT_ALLOW_TOOLS, VM_DISCOVERY_TOOLS,
};

/// Editor-backed application adapter for runtime's proposal presentation
/// port. The runtime crate owns only the typed contract and never names this
/// UI implementation.
pub(crate) struct EditorArtifactProposalHost;

#[async_trait::async_trait]
impl crate::runtime::ArtifactProposalHost for EditorArtifactProposalHost {
    async fn propose_artifact(
        &self,
        language: &str,
        intent: &str,
        source: &str,
    ) -> anyhow::Result<crate::runtime::ArtifactProposalDecision> {
        Ok(
            match propose_artifact_with_decision(language, intent, source).await? {
                ProposalDecision::Execute { source } => {
                    crate::runtime::ArtifactProposalDecision::Execute { source }
                }
                ProposalDecision::Chat { context } => {
                    crate::runtime::ArtifactProposalDecision::Chat { context }
                }
                ProposalDecision::Cancel => crate::runtime::ArtifactProposalDecision::Cancel,
            },
        )
    }
}
pub use todo::{todo_journal, TodoJournalReceiver, TodoJournalTarget, TodoJournalWriter, TodoList};
pub use types::{
    ContentBlock, EffectAuditAuthority, HostModeState, LiveOutput, LiveOutputSink, ToolContext,
    ToolDefinition, ToolInputSchema, ToolResult, ToolUse,
};

pub(crate) use implementations::patch::preview_patched_text;
pub(crate) use implementations::propose::{
    resume_terminal_after_editor, run_editor, suspend_terminal_for_editor,
};
pub(crate) use implementations::restart::{
    deferred_frontend_restart_from_tool_result, frontend_replacement_args,
};
