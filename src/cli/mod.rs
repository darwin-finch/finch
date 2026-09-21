// CLI module
// Public interface for command-line interface

mod chatgpt_auth;
mod commands;
mod conversation;
mod conversation_compactor; // Infinite context: summarise dropped messages
mod diff;
mod global_output; // Phase 3.5: Global output system with macros
mod grok_auth;
mod input;
mod llm_dialogs; // LLM-prompted user dialogs (AskUserQuestion)
mod memtree_console; // Phase 4+: Tree-structured conversation interface
mod mention_session;
mod menu;
mod messages; // Trait-based polymorphic message system
mod output_layer; // Phase 3.5: Tracing integration
mod output_manager;
mod repl;
mod repl_event; // Phase 2-3: Event loop infrastructure
mod setup_wizard; // First-run setup wizard (API keys + device selection)
mod status_bar;
#[cfg(test)]
pub(crate) mod test_projection;
mod tui;
mod usage; // Phase 2: Terminal UI

pub use chatgpt_auth::{
    render_status_line as render_chatgpt_auth_status_line,
    save_named_credential as save_chatgpt_named_credential, ChatGptAuthService,
    DeviceLoginPresentation as ChatGptDeviceLoginPresentation,
};
pub use commands::handle_command;
pub use conversation::ConversationHistory;
pub(crate) use diff::{
    render_files, sanitize_multiline, sanitize_terminal, summarize_files, DiffColorMode, DiffHunk,
    DiffLine, DiffLineKind, FileDiff, MAX_DIFF_HUNKS, MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES,
    MAX_DIFF_LINE_CHARS,
};
pub use global_output::{
    get_global_tui_renderer, global_output, global_status, is_non_interactive, logging_enabled,
    set_global_output, set_global_status, set_global_tui_renderer, shutdown_global_tui,
};
pub use grok_auth::{
    render_status_line as render_grok_auth_status_line,
    save_named_credential as save_grok_named_credential,
    DeviceLoginPresentation as GrokDeviceLoginPresentation, GrokAuthService,
};
pub use input::InputHandler;
pub use llm_dialogs::{
    build_annotations, AnnotationEntry, AskUserQuestionInput, AskUserQuestionOutput, Question,
    QuestionOption,
};
pub use memtree_console::{ConsoleNode, ConsoleNodeType, MemTreeConsole};
pub use messages::{Message, MessageId, MessageRef, MessageStatus, WorkUnit};
pub use messages::{
    ProgressMessage, StaticMessage, StreamingResponseMessage, ToolExecutionMessage,
    UserQueryMessage,
};
pub(crate) use output_layer::MessageVisitor;
pub use output_layer::OutputManagerLayer;
pub use output_manager::{OutputManager, VmOutputProjection};
pub use repl::{Repl, ReplMode, ReplModeState};
pub use repl_event::brain_selection::{
    resolve_selection, EffectiveSelection, SelectionRequest, SelectionSource,
};
pub use repl_event::{format_token_count, format_tool_label, EventLoop, ReplEvent};
pub(crate) use repl_event::{
    is_tool_allowed_in_mode, PLANNING_ALLOWED_TOOLS, PLANNING_ALLOWED_TOOL_ALIASES,
};
pub use setup_wizard::{
    show_setup_wizard, validate_command_and_apply, validate_first_run_and_apply, SetupApplyOutcome,
    SetupResult,
};
pub use status_bar::{StatusBar, StatusLine, StatusLineType};
pub use tui::{
    ActivityUsage, Dialog, DialogOption, DialogResult, QuestionOptionView, QuestionView,
    TabbedDialog, TabbedDialogResult, TuiOutputPort, TuiRenderer, TuiStatusPort,
};
