// Legacy conversation-logging compatibility API.
//
// Automatic query/response persistence is disabled. Explicit ratings use the
// private, bounded FeedbackLogger instead.

pub mod conversation_logger;

pub use conversation_logger::{ConversationLogger, Feedback, LogEntry};
