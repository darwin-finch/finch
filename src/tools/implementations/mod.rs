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

// Active learning tools (Phase 2)
pub mod analyze_model;
pub mod compare_responses;
pub mod generate_training;
pub mod query_local;
pub mod train;

// Re-exports for convenience
pub use bash::BashTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use restart::RestartTool;
pub use save_and_exec::SaveAndExecTool;
pub use web_fetch::WebFetchTool;

// Active learning tools
pub use analyze_model::AnalyzeModelTool;
pub use compare_responses::CompareResponsesTool;
pub use generate_training::GenerateTrainingDataTool;
pub use query_local::QueryLocalModelTool;
pub use train::TrainTool;
