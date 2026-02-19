// CLI module
// Public interface for command-line interface

mod commands;
mod conversation;
pub mod global_output; // Phase 3.5: Global output system with macros
mod input;
pub mod llm_dialogs; // LLM-prompted user dialogs (AskUserQuestion)
pub mod memtree_console; // Phase 4+: Tree-structured conversation interface
pub mod menu;
pub mod messages; // Trait-based polymorphic message system
pub mod output_layer; // Phase 3.5: Tracing integration
mod output_manager;
mod repl;
pub mod repl_event; // Phase 2-3: Event loop infrastructure
pub mod setup_wizard; // First-run setup wizard (API keys + device selection)
mod status_bar;
pub mod suggestions; // Contextual prompt suggestions (like Claude Code)
pub mod tui; // Phase 2: Terminal UI

pub use commands::handle_command;
pub use conversation::ConversationHistory;
pub use input::InputHandler;
pub use llm_dialogs::{AskUserQuestionInput, AskUserQuestionOutput, Question, QuestionOption};
pub use memtree_console::{ConsoleNode, ConsoleNodeType, MemTreeConsole};
pub use messages::{Message, MessageId, MessageRef, MessageStatus};
pub use messages::{ProgressMessage, StaticMessage, StreamingResponseMessage, ToolExecutionMessage, UserQueryMessage};
pub use output_manager::OutputManager;
pub use repl::{Repl, ReplMode};
pub use setup_wizard::show_setup_wizard;
pub use status_bar::{StatusBar, StatusLine, StatusLineType};
pub use suggestions::{Suggestion, SuggestionContext, SuggestionManager, SuggestionSource};
