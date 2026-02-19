// Conversation logging for future LoRA training
//
// Logs all interactions (query + response + metadata) to JSONL format
// for later use in fine-tuning when ONNX/CoreML supports LoRA.

pub mod conversation_logger;

pub use conversation_logger::{ConversationLogger, Feedback, LogEntry};
