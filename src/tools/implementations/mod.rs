// Tool implementations
//
// Concrete implementations of various tools

// Read-only tools
pub mod glob;
pub mod grep;
pub mod read;

// Network tools
pub mod web_fetch;

// Command execution
pub mod bash;

// Self-improvement tools
pub mod restart;
pub mod save_and_exec;

// Plan mode tools
pub mod enter_plan_mode;
pub mod present_plan;

// User interaction tools
pub mod ask_user_question;

// GUI automation tools (macOS only)
#[cfg(target_os = "macos")]
pub mod gui;

// LLM delegation tools (Phase 1)
pub mod llm_tools;

// Memory tools (Phase 4)
pub mod memory_tools;

// Re-exports for convenience
pub use ask_user_question::AskUserQuestionTool;
pub use bash::BashTool;
pub use enter_plan_mode::EnterPlanModeTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use present_plan::PresentPlanTool;
pub use read::ReadTool;
pub use restart::RestartTool;
pub use save_and_exec::SaveAndExecTool;
pub use web_fetch::WebFetchTool;

#[cfg(target_os = "macos")]
pub use gui::{GuiClickTool, GuiInspectTool, GuiTypeTool};

pub use llm_tools::LLMDelegationTool;

pub use memory_tools::{CreateMemoryTool, ListRecentTool, SearchMemoryTool};
