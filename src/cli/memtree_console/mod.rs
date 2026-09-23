// MemTree Console module
//
// Tree-structured conversation interface

mod console;
mod event_handler;

pub use console::{ConsoleNode, ConsoleNodeType, MemTreeConsole, NodeId};
pub use event_handler::EventHandler;
