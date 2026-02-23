// IMPCPD loop runner — iterative plan generation + adversarial critique

use anyhow::{Context, Result};
use crossterm::style::Stylize;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::cli::OutputManager;
use crate::cli::tui::{Dialog, DialogOption, DialogResult, TuiRenderer};
use crate::claude::Message;
use crate::generators::Generator;

use super::personas::select_active_personas;
use super::types::{
    ConvergenceResult, CritiqueItem, ImpcpdConfig, PlanIteration, PlanResult, UserFeedback,
};
use super::IMPCPD_METHODOLOGY;

/// The IMPCPD plan loop.
///
/// Drives iterative plan generation and adversarial multi-persona critique
/// until one of the following conditions is met:
/// - The plan converges (character delta < threshold, no must-address items)
/// - The user approves the plan mid-loop
/// - The hard iteration cap is reached
/// - The user cancels
pub struct PlanLoop {
    generator: Arc<dyn Generator>,
    output_manager: Arc<OutputManager>,
    config: ImpcpdConfig,
}

impl PlanLoop {
    pub fn new(
        generator: Arc<dyn Generator>,
        output_manager: Arc<OutputManager>,
        config: ImpcpdConfig,
    ) -> Self {
        Self {
            generator,
            output_manager,
            config,
        }
    }

    /// Run the full IMPCPD loop for a planning task.
    ///
    /// `task` — the user's task description (e.g. "add JWT auth to the route handler")
    /// `tui`  — shared TUI renderer for showing blocking dialogs
    pub async fn run(
        &self,
        task: &str,
        tui: Arc<Mutex<TuiRenderer>>,
    ) -> Result<PlanResult> {
        let mut history: Vec<PlanIteration> = Vec::new();
        let mut steering_feedback: Option<String> = None;

        for iteration in 1..=self.config.max_iterations {
            // ── 1. Generate plan ────────────────────────────────────────────
            self.show_iteration_header(iteration);
            let plan = self
                .generate_plan(task, &history, steering_feedback.as_deref())
                .await
                .context("Failed to generate plan")?;
            self.show_plan(&plan, iteration);

            // ── 2. Select and display active personas ───────────────────────
            let personas = select_active_personas(&plan);
            self.show_critique_header(iteration, &personas);

            // ── 3. Critique the plan ────────────────────────────────────────
            let critiques = self
                .critique_plan(&plan, &personas)
                .await
                .context("Failed to critique plan")?;
            self.show_critiques(&critiques);

            // ── 4. Check convergence (only from iteration 2 onwards) ────────
            if iteration > 1 {
                if let Some(prev) = history.last() {
                    match check_convergence(&prev.plan_text, &plan, &critiques, &self.config) {
                        ConvergenceResult::Stable { delta_pct } => {
                            self.output_manager.write_info(format!(
                                "\n{} Plan converged after {} iterations (delta: {:.1}%)",
                                "✓".green().bold(),
                                iteration,
                                delta_pct
                            ));
                            history.push(PlanIteration {
                                iteration,
                                plan_text: plan,
                                critiques,
                                user_feedback: None,
                            });
                            return Ok(PlanResult::Converged { iterations: history });
                        }
                        ConvergenceResult::ScopeRunaway => {
                            self.output_manager.write_info(format!(
                                "\n{} Plan grew >40% without resolving critical issues. \
                                 Consider narrowing the scope.",
                                "⚠".yellow().bold()
                            ));
                            // Fall through to user prompt — let the user decide
                        }
                        ConvergenceResult::Continuing => {}
                    }
                }
            }

            // ── 5. Surface minority risks separately ────────────────────────
            let minority: Vec<&CritiqueItem> =
                critiques.iter().filter(|c| c.is_minority_risk).collect();
            if !minority.is_empty() {
                self.show_minority_risks(&minority);
            }

            // ── 6. Prompt user for steering or approval ──────────────────────
            let feedback = self
                .prompt_user_feedback(&tui, iteration)
                .await
                .context("Failed to get user feedback")?;

            match feedback {
                UserFeedback::Approve => {
                    history.push(PlanIteration {
                        iteration,
                        plan_text: plan,
                        critiques,
                        user_feedback: None,
                    });
                    return Ok(PlanResult::UserApproved { iterations: history });
                }
                UserFeedback::Cancel => {
                    return Ok(PlanResult::Cancelled);
                }
                UserFeedback::Continue(text) => {
                    steering_feedback = text.clone();
                    history.push(PlanIteration {
                        iteration,
                        plan_text: plan,
                        critiques,
                        user_feedback: text,
                    });
                }
            }
        }

        // Hard cap reached — return the last plan
        Ok(PlanResult::IterationCap { iterations: history })
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    /// Generate a plan draft (or revision) from the generator.
    async fn generate_plan(
        &self,
        task: &str,
        history: &[PlanIteration],
        feedback: Option<&str>,
    ) -> Result<String> {
        let mut messages: Vec<Message> = Vec::new();

        // Build the initial planning request
        let mut initial = format!(
            "{alignment}\n\n\
             You are an expert software engineer creating an implementation plan.\n\
             Task: {task}\n\n\
             Generate a clear, numbered implementation plan. Requirements:\n\
             - Each step must be specific and actionable\n\
             - Name the exact files to modify or create\n\
             - Keep the scope tight — only what is necessary for this task\n\
             - Steps should leave the codebase in a compilable state when followed in order\n\n\
             Return ONLY the numbered plan. No preamble. No post-amble.",
            alignment = crate::providers::UNIVERSAL_ALIGNMENT_PROMPT.trim(),
        );

        // If there are previous iterations, append the revision history
        if !history.is_empty() {
            initial.push_str("\n\n---\n\nRevision context (previous iterations):");
            for iter in history {
                initial.push_str(&format!(
                    "\n\nIteration {} plan:\n{}",
                    iter.iteration, iter.plan_text
                ));

                // Include the must-address critiques so the LLM knows what to fix
                let must_address: Vec<&CritiqueItem> =
                    iter.critiques.iter().filter(|c| c.is_must_address).collect();
                if !must_address.is_empty() {
                    initial.push_str("\n\nMust-address issues from critique:");
                    for item in must_address {
                        let step = item
                            .step_ref
                            .map(|s| format!(" (step {})", s))
                            .unwrap_or_default();
                        initial.push_str(&format!(
                            "\n- [{}{}] {}",
                            item.persona, step, item.concern
                        ));
                    }
                }

                if let Some(fb) = &iter.user_feedback {
                    initial.push_str(&format!("\n\nUser steering: {fb}"));
                }
            }
            initial.push_str(
                "\n\n---\n\n\
                 Generate a revised plan that addresses all must-address issues above.",
            );
        }

        if let Some(fb) = feedback {
            initial.push_str(&format!("\n\nAdditional user direction: {fb}"));
        }

        messages.push(Message::user(initial));

        let response = self
            .generator
            .generate(messages, None)
            .await
            .context("Generator failed during plan generation")?;

        Ok(response.text.trim().to_string())
    }

    /// Critique the plan using the active personas and the embedded IMPCPD methodology.
    async fn critique_plan(
        &self,
        plan: &str,
        personas: &[&str],
    ) -> Result<Vec<CritiqueItem>> {
        let prompt = format!(
            "{alignment}\n\n\
             {methodology}\n\n\
             Active personas for this critique: {persona_list}\n\n\
             ---\n\n\
             Critique the following implementation plan:\n\n\
             {plan}\n\n\
             Return a JSON array of critique items only. \
             Use exactly the schema described in the methodology. \
             Do not wrap in markdown code fences. \
             If there are no issues, return [].",
            alignment = crate::providers::UNIVERSAL_ALIGNMENT_PROMPT.trim(),
            methodology = IMPCPD_METHODOLOGY,
            persona_list = personas.join(", "),
            plan = plan,
        );

        let messages = vec![Message::user(prompt)];

        let response = self
            .generator
            .generate(messages, None)
            .await
            .context("Generator failed during critique")?;

        parse_critique_response(&response.text)
    }

    /// Show the header for a new iteration
    fn show_iteration_header(&self, iteration: usize) {
        self.output_manager.write_info(format!(
            "\n{} Iteration {}/{}",
            "▸".cyan().bold(),
            iteration,
            self.config.max_iterations
        ));
        self.output_manager.write_info(format!(
            "{}",
            "─".repeat(60).dark_grey()
        ));
    }

    /// Display the current plan draft in the output area
    fn show_plan(&self, plan: &str, iteration: usize) {
        self.output_manager
            .write_info(format!("\n📋 Plan v{}:\n", iteration));
        self.output_manager.write_info(plan.to_string());
        self.output_manager.write_info(String::new());
    }

    /// Show which personas are active for this critique pass
    fn show_critique_header(&self, _iteration: usize, personas: &[&str]) {
        self.output_manager.write_info(format!(
            "🔍 Running critique: {}",
            personas.join(" · ").cyan()
        ));
    }

    /// Display critique results, grouped into must-address vs other
    fn show_critiques(&self, critiques: &[CritiqueItem]) {
        if critiques.is_empty() {
            self.output_manager.write_info(format!(
                "  {} No issues found.",
                "✓".green()
            ));
            return;
        }

        let must_address: Vec<&CritiqueItem> =
            critiques.iter().filter(|c| c.is_must_address).collect();
        let other: Vec<&CritiqueItem> = critiques
            .iter()
            .filter(|c| !c.is_must_address && !c.is_minority_risk)
            .collect();

        if !must_address.is_empty() {
            self.output_manager.write_info(format!(
                "\n  {} Must-address ({}):",
                "⚠".red().bold(),
                must_address.len()
            ));
            for item in &must_address {
                let step_label = item
                    .step_ref
                    .map(|s| format!(" [step {}]", s))
                    .unwrap_or_default();
                self.output_manager.write_info(format!(
                    "  • {} {}{} — {} (s:{}/c:{})",
                    item.persona.clone().red().bold(),
                    step_label,
                    String::new(),
                    item.concern,
                    item.severity,
                    item.confidence
                ));
            }
        }

        if !other.is_empty() {
            self.output_manager.write_info(format!(
                "\n  {} Other concerns ({}):",
                "ℹ".yellow(),
                other.len()
            ));
            for item in &other {
                let step_label = item
                    .step_ref
                    .map(|s| format!(" [step {}]", s))
                    .unwrap_or_default();
                self.output_manager.write_info(format!(
                    "  • {} {}{} — {} (s:{}/c:{})",
                    item.persona.clone().yellow(),
                    step_label,
                    String::new(),
                    item.concern,
                    item.severity,
                    item.confidence
                ));
            }
        }
    }

    /// Surface minority risks (high severity, low confidence) separately
    fn show_minority_risks(&self, minority: &[&CritiqueItem]) {
        self.output_manager.write_info(format!(
            "\n  {} Minority risks (high severity, low confidence — worth noting):",
            "◈".blue()
        ));
        for item in minority {
            let step_label = item
                .step_ref
                .map(|s| format!(" [step {}]", s))
                .unwrap_or_default();
            self.output_manager.write_info(format!(
                "  ◈ {} {}{} — {} (s:{}/c:{})",
                item.persona.clone().blue(),
                step_label,
                String::new(),
                item.concern,
                item.severity,
                item.confidence
            ));
        }
    }

    /// Show a blocking dialog and collect user steering feedback.
    async fn prompt_user_feedback(
        &self,
        tui: &Arc<Mutex<TuiRenderer>>,
        iteration: usize,
    ) -> Result<UserFeedback> {
        let title = format!(
            "Iteration {}/{} complete — what next?",
            iteration, self.config.max_iterations
        );

        let options = vec![
            DialogOption::with_description(
                "Continue",
                "Run another critique pass (or type feedback below)",
            ),
            DialogOption::with_description("Approve", "Accept the current plan as-is"),
            DialogOption::with_description("Cancel", "Abandon planning, return to normal mode"),
        ];

        let dialog = Dialog::select_with_custom(title, options).with_help(
            "↑↓/j/k = navigate · Enter = select · 'o' = type steering feedback · Esc = cancel",
        );

        let result = {
            let mut tui_guard = tui.lock().await;
            tui_guard
                .show_dialog(dialog)
                .context("Failed to show steering dialog")?
        };

        let feedback = match result {
            DialogResult::Selected(1) => UserFeedback::Approve,
            DialogResult::Selected(2) | DialogResult::Cancelled => UserFeedback::Cancel,
            DialogResult::CustomText(text) if !text.trim().is_empty() => {
                UserFeedback::Continue(Some(text.trim().to_string()))
            }
            _ => UserFeedback::Continue(None),
        };

        Ok(feedback)
    }
}

// ── Convergence check ──────────────────────────────────────────────────────────

/// Check whether successive plan iterations have converged.
///
/// Convergence requires:
/// 1. The character delta between iterations is below `config.convergence_pct`
/// 2. There are no `is_must_address` items in the critique
fn check_convergence(
    prev: &str,
    curr: &str,
    critiques: &[CritiqueItem],
    config: &ImpcpdConfig,
) -> ConvergenceResult {
    let delta = char_delta_pct(prev, curr);
    let must_count = critiques.iter().filter(|c| c.is_must_address).count();

    if delta < config.convergence_pct && must_count == 0 {
        return ConvergenceResult::Stable { delta_pct: delta };
    }

    // Scope runaway: plan grew >40% AND there are still must-address items
    if curr.len() > (prev.len() as f32 * 1.4) as usize && must_count > 0 {
        return ConvergenceResult::ScopeRunaway;
    }

    ConvergenceResult::Continuing
}

/// Compute the percentage change in character count between two strings.
fn char_delta_pct(prev: &str, curr: &str) -> f32 {
    if prev.is_empty() {
        return 100.0;
    }
    let diff = (curr.len() as i64 - prev.len() as i64).unsigned_abs() as f32;
    diff / prev.len() as f32 * 100.0
}

// ── JSON critique parsing ──────────────────────────────────────────────────────

/// Parse the LLM critique response into a Vec<CritiqueItem>.
///
/// The LLM is asked to return a bare JSON array, but may wrap it in markdown
/// code fences. This function strips fences and attempts to parse.
/// On failure it returns an empty vec (soft degradation).
fn parse_critique_response(text: &str) -> Result<Vec<CritiqueItem>> {
    let stripped = strip_markdown_fences(text.trim());

    // Try direct parse
    if let Ok(items) = serde_json::from_str::<Vec<RawCritiqueItem>>(stripped) {
        return Ok(items.into_iter().map(CritiqueItem::from).collect());
    }

    // Try to find a JSON array within the text
    if let Some(start) = stripped.find('[') {
        if let Some(end) = stripped.rfind(']') {
            let slice = &stripped[start..=end];
            if let Ok(items) = serde_json::from_str::<Vec<RawCritiqueItem>>(slice) {
                return Ok(items.into_iter().map(CritiqueItem::from).collect());
            }
        }
    }

    // Soft degradation: log a warning and return empty
    tracing::warn!("Failed to parse critique JSON response; treating as no issues");
    Ok(vec![])
}

/// Strip leading/trailing markdown code fences (```json ... ``` or ``` ... ```)
fn strip_markdown_fences(s: &str) -> &str {
    let s = s.trim();
    let s = if let Some(rest) = s.strip_prefix("```json") {
        rest
    } else if let Some(rest) = s.strip_prefix("```") {
        rest
    } else {
        s
    };
    if let Some(rest) = s.strip_suffix("```") {
        rest.trim()
    } else {
        s.trim()
    }
}

/// Raw JSON shape from the LLM — allows missing/optional fields
#[derive(Debug, serde::Deserialize)]
struct RawCritiqueItem {
    persona: String,
    concern: String,
    step_ref: Option<usize>,
    severity: u8,
    confidence: u8,
    // signal / is_must_address / is_minority_risk can be ignored — we recompute
    #[serde(default)]
    signal: Option<u16>,
    #[serde(default)]
    is_must_address: Option<bool>,
    #[serde(default)]
    is_minority_risk: Option<bool>,
}

impl From<RawCritiqueItem> for CritiqueItem {
    fn from(raw: RawCritiqueItem) -> Self {
        // Always recompute derived fields from raw severity/confidence
        // (never trust what the LLM computed — it sometimes gets it wrong)
        let _ = (raw.signal, raw.is_must_address, raw.is_minority_risk);
        CritiqueItem::new(raw.persona, raw.concern, raw.step_ref, raw.severity, raw.confidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_char_delta_pct_no_change() {
        assert!((char_delta_pct("hello", "hello") - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_char_delta_pct_doubled() {
        // "ab" → "abcd" = 2 chars added out of 2 = 100%
        assert!((char_delta_pct("ab", "abcd") - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_char_delta_pct_empty_prev() {
        assert!((char_delta_pct("", "anything") - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_convergence_stable_no_issues() {
        let cfg = ImpcpdConfig::default();
        let result = check_convergence("hello world", "hello world", &[], &cfg);
        assert!(matches!(result, ConvergenceResult::Stable { delta_pct } if delta_pct < 1.0));
    }

    #[test]
    fn test_convergence_stable_blocked_by_must_address() {
        let cfg = ImpcpdConfig::default();
        let critiques = vec![CritiqueItem::new("Security", "Missing auth", None, 9, 8)];
        // Plan unchanged, but there's a must-address item → not stable
        let result = check_convergence("hello world", "hello world", &critiques, &cfg);
        assert!(!matches!(result, ConvergenceResult::Stable { .. }));
    }

    #[test]
    fn test_convergence_scope_runaway() {
        let cfg = ImpcpdConfig::default();
        let prev = "short";
        let curr = "a".repeat(10); // much longer
        let critiques = vec![CritiqueItem::new("Regression", "Breaks thing", None, 9, 9)];
        let result = check_convergence(prev, &curr, &critiques, &cfg);
        assert!(matches!(result, ConvergenceResult::ScopeRunaway));
    }

    #[test]
    fn test_strip_markdown_fences_json() {
        let s = "```json\n[{\"a\":1}]\n```";
        assert_eq!(strip_markdown_fences(s), "[{\"a\":1}]");
    }

    #[test]
    fn test_strip_markdown_fences_plain() {
        let s = "```\n[{\"a\":1}]\n```";
        assert_eq!(strip_markdown_fences(s), "[{\"a\":1}]");
    }

    #[test]
    fn test_strip_markdown_fences_no_fences() {
        let s = "[{\"a\":1}]";
        assert_eq!(strip_markdown_fences(s), "[{\"a\":1}]");
    }

    #[test]
    fn test_parse_critique_response_valid_json() {
        let json = r#"[
            {"persona":"Security","concern":"Missing validation","step_ref":2,"severity":9,"confidence":8,"signal":72,"is_must_address":true,"is_minority_risk":false}
        ]"#;
        let items = parse_critique_response(json).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].persona, "Security");
        assert!(items[0].is_must_address);
    }

    #[test]
    fn test_parse_critique_response_with_fences() {
        let json = "```json\n[{\"persona\":\"Regression\",\"concern\":\"May break X\",\"step_ref\":null,\"severity\":7,\"confidence\":6,\"signal\":42,\"is_must_address\":false,\"is_minority_risk\":false}]\n```";
        let items = parse_critique_response(json).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].persona, "Regression");
    }

    #[test]
    fn test_parse_critique_response_empty_array() {
        let items = parse_critique_response("[]").unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_critique_response_invalid_json_soft_degrades() {
        let items = parse_critique_response("not json at all").unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_raw_critique_recomputes_derived_fields() {
        // Provide wrong is_must_address=false but severity=9 confidence=8 → should be recomputed
        let json = r#"[{"persona":"Arch","concern":"X","step_ref":null,"severity":9,"confidence":8,"signal":72,"is_must_address":false,"is_minority_risk":false}]"#;
        let items = parse_critique_response(json).unwrap();
        assert!(items[0].is_must_address); // recomputed to true
    }
}
