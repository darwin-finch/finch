// CLI module
// Public interface for command-line interface

mod commands;
mod conversation;
mod input;
pub mod menu;
mod output_manager;
mod repl;
mod status_bar;

pub use commands::handle_command;
pub use conversation::ConversationHistory;
pub use input::InputHandler;
pub use output_manager::{OutputManager, OutputMessage};
pub use repl::Repl;
pub use status_bar::{StatusBar, StatusLine, StatusLineType};
