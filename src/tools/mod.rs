// Tool execution system for local tool use
//
// Enables Shammah to execute tools (WebFetch, Bash, Read, etc.) locally
// instead of only generating text responses.

mod executor;
mod implementations;
mod mcp;
mod pattern_matcher;
mod patterns;
mod permissions;
mod registry;
mod semantic;
mod todo;
mod tool_loop;
mod types;

pub use crate::brain::{
    BrainTask as TodoItem, BrainTaskPriority as TodoPriority, BrainTaskStatus as TodoStatus,
};
pub use executor::{generate_tool_signature, ApprovalSource, ToolExecutor, ToolSignature};
pub use implementations::llm_tools::create_llm_tools;
pub use implementations::propose::{propose_artifact_with_decision, ProposalDecision};
pub use implementations::restart::DeferredFrontendRestart;
pub use implementations::{
    AgentAwaitTool, AgentCancelTool, AgentPollTool, AgentSpawnTool, AnsibleTool,
    AskUserQuestionTool, BashTool, CreateMemoryTool, EditTool, EnterPlanModeTool,
    GetLanguageDefinitionTool, GetVmStateTool, GlobTool, GrepTool, HashCompareTool,
    InspectMemoryTool, InspectProgramTool, InspectVmWordTool, InspectWordTool, LLMDelegationTool,
    ListRecentTool, PatchTool, PresentPlanTool, ReadTool, RestartTool, SearchMemoryTool,
    SearchVmVocabularyTool, SearchVocabularyTool, SearchWordTool, SubmitProgramTool, TodoReadTool,
    TodoWriteTool, WebFetchTool, WriteTool,
};
#[cfg(target_os = "macos")]
pub use implementations::{
    ExcelActivateTool, ExcelFormulaTool, ExcelRangeTool, ExcelReadTool, ExcelSheetsTool,
    ExcelWriteTool, GuiClickTool, GuiInspectTool, GuiTypeTool,
};
pub use mcp::{McpClient, McpConnection, McpServerConfig, McpToolDescriptor, TransportType};
pub use pattern_matcher::ToolPatternMatcher;
pub use patterns::{ExactApproval, MatchType, PatternType, PersistentPatternStore, ToolPattern};
pub use permissions::{
    refined_effect_for_approval, PermissionCheck, PermissionManager, PermissionRule,
    ToolPermissionConfig, PEER_HARD_DENY_TOOLS, PEER_REVIEWED_CHANGESET_TOOLS,
    PEER_SILENT_ALLOW_TOOLS, VM_DISCOVERY_TOOLS,
};
pub use registry::{Tool, ToolRegistry};
pub use semantic::{
    compile_policy_from_registry, semantic_tools_for_advertisement, tool_authority_from_effect,
};
pub use todo::{todo_journal, TodoJournalReceiver, TodoJournalTarget, TodoJournalWriter, TodoList};
pub use tool_loop::{
    AdmitError, ObserveOutcome, PreparedCall, RejectReason, RejectedCall, ToolCatalog, ToolLoop,
    ToolLoopIdentity, ToolLoopResult, ToolLoopTerminal, ValidatedCall,
};
pub use types::{
    ContentBlock, LiveOutput, LiveOutputSink, ToolContext, ToolDefinition, ToolInputSchema,
    ToolResult, ToolUse,
};

pub(crate) use implementations::propose::{
    resume_terminal_after_editor, run_editor, suspend_terminal_for_editor,
};
pub(crate) use implementations::restart::{
    deferred_frontend_restart_from_tool_result, frontend_replacement_args,
};
