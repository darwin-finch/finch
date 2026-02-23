// IMCPD types — CritiqueItem, PlanIteration, ConvergenceResult, ImcpdConfig

use serde::{Deserialize, Serialize};

/// A single critique concern from one persona
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CritiqueItem {
    /// The persona that raised this concern (e.g. "Security", "Regression")
    pub persona: String,
    /// Human-readable description of the issue
    pub concern: String,
    /// Which step number (1-indexed) in the plan this concern refers to, if any
    pub step_ref: Option<usize>,
    /// Severity score 1–10 (impact if not addressed)
    pub severity: u8,
    /// Confidence score 1–10 (certainty the issue exists)
    pub confidence: u8,
    /// Pre-computed signal strength: severity × confidence
    pub signal: u16,
    /// severity ≥ 8 AND confidence ≥ 7
    pub is_must_address: bool,
    /// severity ≥ 7 AND confidence ≤ 4 (high risk but low certainty — worth surfacing)
    pub is_minority_risk: bool,
}

impl CritiqueItem {
    /// Build a CritiqueItem, computing derived fields automatically.
    pub fn new(
        persona: impl Into<String>,
        concern: impl Into<String>,
        step_ref: Option<usize>,
        severity: u8,
        confidence: u8,
    ) -> Self {
        let s = severity.min(10);
        let c = confidence.min(10);
        Self {
            persona: persona.into(),
            concern: concern.into(),
            step_ref,
            severity: s,
            confidence: c,
            signal: s as u16 * c as u16,
            is_must_address: s >= 8 && c >= 7,
            is_minority_risk: s >= 7 && c <= 4,
        }
    }
}

/// One complete plan iteration: draft text + critique received + optional user feedback
#[derive(Debug, Clone)]
pub struct PlanIteration {
    pub iteration: usize,
    pub plan_text: String,
    pub critiques: Vec<CritiqueItem>,
    /// User steering feedback entered after this iteration, if any
    pub user_feedback: Option<String>,
}

/// Result of a convergence check between two successive plan iterations
#[derive(Debug, Clone)]
pub enum ConvergenceResult {
    /// Plan is stable: character delta below threshold AND no must-address items
    Stable { delta_pct: f32 },
    /// Plan grew >40% without resolving must-address items
    ScopeRunaway,
    /// Not yet converged — continue to next iteration
    Continuing,
}

/// Result returned from `PlanLoop::run()`
#[derive(Debug)]
pub enum PlanResult {
    /// Plan converged (stable delta, no must-address items) — final plan in last iteration
    Converged { iterations: Vec<PlanIteration> },
    /// User explicitly approved the plan mid-loop
    UserApproved { iterations: Vec<PlanIteration> },
    /// User cancelled planning
    Cancelled,
    /// Hard iteration cap reached — final plan in last iteration
    IterationCap { iterations: Vec<PlanIteration> },
}

impl PlanResult {
    /// Return the last plan text, regardless of how the loop ended.
    pub fn final_plan(&self) -> Option<&str> {
        let iters = match self {
            PlanResult::Converged { iterations }
            | PlanResult::UserApproved { iterations }
            | PlanResult::IterationCap { iterations } => iterations,
            PlanResult::Cancelled => return None,
        };
        iters.last().map(|i| i.plan_text.as_str())
    }
}

/// Configuration for the IMCPD loop
#[derive(Debug, Clone)]
pub struct ImcpdConfig {
    /// Maximum number of plan-critique iterations before stopping
    pub max_iterations: usize,
    /// Percentage character delta below which the plan is considered stable
    pub convergence_pct: f32,
}

impl Default for ImcpdConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            convergence_pct: 15.0,
        }
    }
}

/// Internal user feedback result from the steering dialog
#[derive(Debug)]
pub enum UserFeedback {
    /// User approved the current plan — stop iterating
    Approve,
    /// User cancelled planning entirely
    Cancel,
    /// Continue to next iteration, optionally with steering text
    Continue(Option<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_critique_item_derived_fields() {
        let item = CritiqueItem::new("Security", "Missing auth check", Some(3), 9, 8);
        assert_eq!(item.signal, 72);
        assert!(item.is_must_address);
        assert!(!item.is_minority_risk);
    }

    #[test]
    fn test_critique_item_minority_risk() {
        let item = CritiqueItem::new("Architecture", "Possible circular dep", None, 7, 3);
        assert_eq!(item.signal, 21);
        assert!(!item.is_must_address);
        assert!(item.is_minority_risk);
    }

    #[test]
    fn test_critique_item_neither() {
        let item = CritiqueItem::new("Completeness", "Missing test step", None, 5, 8);
        assert!(!item.is_must_address); // severity < 8
        assert!(!item.is_minority_risk); // confidence > 4
    }

    #[test]
    fn test_critique_item_clamps_scores() {
        let item = CritiqueItem::new("Regression", "Overflow", None, 15, 12);
        assert_eq!(item.severity, 10);
        assert_eq!(item.confidence, 10);
        assert_eq!(item.signal, 100);
    }

    #[test]
    fn test_plan_result_final_plan_cancelled() {
        let result = PlanResult::Cancelled;
        assert!(result.final_plan().is_none());
    }

    #[test]
    fn test_plan_result_final_plan_converged() {
        let iterations = vec![PlanIteration {
            iteration: 1,
            plan_text: "Step 1: do thing".to_string(),
            critiques: vec![],
            user_feedback: None,
        }];
        let result = PlanResult::Converged { iterations };
        assert_eq!(result.final_plan(), Some("Step 1: do thing"));
    }

    #[test]
    fn test_imcpd_config_defaults() {
        let cfg = ImcpdConfig::default();
        assert_eq!(cfg.max_iterations, 3);
        assert!((cfg.convergence_pct - 15.0).abs() < f32::EPSILON);
    }
}
