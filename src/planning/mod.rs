// IMCPD — Iterative Multi-Perspective Code Plan Debugging
//
// This module drives Finch's /plan command: it generates a numbered implementation
// plan, then runs multi-persona adversarial critique passes until the plan converges
// or the user approves it.

pub mod types;
pub mod personas;
pub mod loop_runner;

pub use types::{
    ConvergenceResult, CritiqueItem, ImcpdConfig, PlanIteration, PlanResult, UserFeedback,
};
pub use personas::select_active_personas;
pub use loop_runner::PlanLoop;

/// The IMCPD runtime methodology spec, embedded at compile time.
///
/// This is sent verbatim to the LLM as the critique system context.
/// The spec defines the six critique personas, their activation rules,
/// the severity×confidence scoring model, and the expected JSON output format.
pub const IMCPD_METHODOLOGY: &str = include_str!("imcpd_methodology.md");
