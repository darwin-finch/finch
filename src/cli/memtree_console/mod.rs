// MemTree Console module
//
// Tree-structured conversation interface

mod console;
mod event_handler;

pub use console::{ConsoleNode, ConsoleNodeType, MemTreeConsole};
pub use event_handler::EventHandler;
