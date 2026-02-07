// CLI module
// Public interface for command-line interface

mod commands;
mod conversation;
pub mod global_output; // Phase 3.5: Global output system with macros
mod input;
pub mod menu;
mod output_manager;
mod repl;
mod status_bar;
pub mod tui; // Phase 2: Terminal UI

pub use commands::handle_command;
pub use conversation::ConversationHistory;
pub use input::InputHandler;
pub use output_manager::{OutputManager, OutputMessage};
pub use repl::Repl;
pub use status_bar::{StatusBar, StatusLine, StatusLineType};
