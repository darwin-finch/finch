// Status Bar - Multi-line status display at bottom of terminal
//
// This module manages the status bar area that shows:
// - Training statistics
// - Download progress
// - Operation status
//
// Supports dynamic addition/removal of status lines.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Types of status lines
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StatusLineType {
    /// Session label shown permanently (e.g. "◆ swift-falcon · ~/repos/finch")
    SessionLabel,
    /// Session-cumulative token burn for this Brain
    /// ("this session: 182k in / 31k out, ~$1.40 est")
    SessionUsage,
    /// Memory context: engine type + recall info ("🧠 neural · 142 memories · recalled 3")
    MemoryContext,
    /// Conversation topic derived from MemTree overall centroid ("📋 <topic>")
    ConversationTopic,
    /// Conversation focus derived from MemTree recency centroid ("   └─ now: <focus>")
    ConversationFocus,
    /// N-th depth-sliced context line (0 = broadest/overall … last = most recent).
    /// Replaces ConversationTopic + ConversationFocus when context_lines > 1.
    ContextLine(usize),
    /// N-th canonical conversation line projected from the attached Brain log.
    /// This is deliberately distinct from semantic-memory/session summaries so
    /// a zero-result recall cannot erase durable conversation history.
    BrainContextLine(usize),
    /// Live query statistics (tokens, latency, model)
    LiveStats,
    /// Active child count and provider-reported child token usage.
    AgentActivity,
    /// Training statistics (queries, local%, quality)
    TrainingStats,
    /// Model download progress
    DownloadProgress,
    /// Current operation status
    OperationStatus,
    /// Contextual suggestions (like Claude Code)
    Suggestions,
    /// Auto-compaction percentage (displayed on right side)
    CompactionPercent,
    /// Custom status line with ID
    Custom(String),
}

/// First-line glyph for a MemTree recap tree.
pub(crate) const RECAP_MEMTREE_ROOT: &str = "📋";
/// First-line glyph for a Brain recap tree.
pub(crate) const RECAP_BRAIN_ROOT: &str = "💬";

const RECAP_NOW_PREFIX: &str = "   └─ now: ";
const RECAP_BRANCH_PREFIX: &str = "   ├─ ";

/// Label one recap line. Only a singleton or the last of several is `now:`.
pub(crate) fn recap_tree_label(index: usize, count: usize, text: &str, root_glyph: &str) -> String {
    let text = recap_line_body(text);
    if count <= 1 || index + 1 == count {
        format!("{RECAP_NOW_PREFIX}{text}")
    } else if index == 0 {
        format!("{root_glyph} {text}")
    } else {
        format!("{RECAP_BRANCH_PREFIX}{text}")
    }
}

fn recap_line_body(content: &str) -> &str {
    content
        .strip_prefix(RECAP_NOW_PREFIX)
        .or_else(|| content.strip_prefix(RECAP_BRANCH_PREFIX))
        .or_else(|| content.strip_prefix("💬 "))
        .or_else(|| content.strip_prefix("📋 "))
        .unwrap_or(content)
}

/// A single status line
#[derive(Debug, Clone)]
pub struct StatusLine {
    /// Type of status line
    pub line_type: StatusLineType,
    /// Content to display
    pub content: String,
}

/// Thread-safe status bar manager
pub struct StatusBar {
    /// Active status lines (keyed by type)
    lines: Arc<RwLock<HashMap<StatusLineType, String>>>,
}

impl StatusBar {
    /// Create a new StatusBar
    pub fn new() -> Self {
        Self {
            lines: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add or update a status line
    pub fn update_line(&self, line_type: StatusLineType, content: impl Into<String>) {
        let mut lines = self.lines.write().unwrap();
        lines.insert(line_type, content.into());
    }

    /// Remove a status line
    pub fn remove_line(&self, line_type: &StatusLineType) {
        let mut lines = self.lines.write().unwrap();
        lines.remove(line_type);
    }

    /// Clear all status lines
    pub fn clear(&self) {
        let mut lines = self.lines.write().unwrap();
        lines.clear();
    }

    /// Get all status lines in a consistent order
    pub fn get_lines(&self) -> Vec<StatusLine> {
        let lines = self.lines.read().unwrap();

        // Order: session/context lines, training stats, child activity,
        // download/operation lines, suggestions, compaction, then custom.
        let mut result = Vec::new();

        // Add in preferred order
        if let Some(content) = lines.get(&StatusLineType::SessionLabel) {
            result.push(StatusLine {
                line_type: StatusLineType::SessionLabel,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::SessionUsage) {
            result.push(StatusLine {
                line_type: StatusLineType::SessionUsage,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::MemoryContext) {
            result.push(StatusLine {
                line_type: StatusLineType::MemoryContext,
                content: content.clone(),
            });
        }

        let has_brain_context = lines
            .keys()
            .any(|key| matches!(key, StatusLineType::BrainContextLine(_)));

        // Legacy topic/focus slots stay hidden once a Brain recap exists.
        // ContextLine + BrainContextLine fold into one recap tree so two
        // singleton projectors cannot each print `└─ now:`.
        if !has_brain_context {
            if let Some(content) = lines.get(&StatusLineType::ConversationTopic) {
                result.push(StatusLine {
                    line_type: StatusLineType::ConversationTopic,
                    content: content.clone(),
                });
            }

            if let Some(content) = lines.get(&StatusLineType::ConversationFocus) {
                result.push(StatusLine {
                    line_type: StatusLineType::ConversationFocus,
                    content: content.clone(),
                });
            }
        }

        let mut recap_entries: Vec<(StatusLineType, String)> = Vec::new();
        let mut ctx_entries: Vec<(usize, String)> = lines
            .iter()
            .filter_map(|(k, v)| {
                if let StatusLineType::ContextLine(n) = k {
                    Some((*n, v.clone()))
                } else {
                    None
                }
            })
            .collect();
        ctx_entries.sort_by_key(|(n, _)| *n);
        let mut brain_entries: Vec<(usize, String)> = lines
            .iter()
            .filter_map(|(key, value)| match key {
                StatusLineType::BrainContextLine(index) => Some((*index, value.clone())),
                _ => None,
            })
            .collect();
        brain_entries.sort_by_key(|(index, _)| *index);

        // A full Brain 💬/now tree still hides the MemTree 📋/now tree.
        // Two singleton `now:` projectors are one recap, not two trees: keep
        // both lines and re-prefix so only the last is `now:`.
        let fold_singleton_now = ctx_entries.len() == 1
            && brain_entries.len() == 1
            && ctx_entries[0].1.contains("└─ now:")
            && brain_entries[0].1.contains("└─ now:");
        if has_brain_context && !fold_singleton_now {
            ctx_entries.clear();
        }

        recap_entries.extend(
            ctx_entries
                .into_iter()
                .map(|(n, content)| (StatusLineType::ContextLine(n), content)),
        );
        recap_entries.extend(
            brain_entries
                .into_iter()
                .map(|(index, content)| (StatusLineType::BrainContextLine(index), content)),
        );

        let recap_count = recap_entries.len();
        let root_glyph = match recap_entries.first().map(|(line_type, _)| line_type) {
            Some(StatusLineType::ContextLine(_)) => RECAP_MEMTREE_ROOT,
            _ => RECAP_BRAIN_ROOT,
        };
        for (index, (line_type, content)) in recap_entries.into_iter().enumerate() {
            result.push(StatusLine {
                line_type,
                content: recap_tree_label(index, recap_count, &content, root_glyph),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::TrainingStats) {
            result.push(StatusLine {
                line_type: StatusLineType::TrainingStats,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::AgentActivity) {
            result.push(StatusLine {
                line_type: StatusLineType::AgentActivity,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::DownloadProgress) {
            result.push(StatusLine {
                line_type: StatusLineType::DownloadProgress,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::OperationStatus) {
            result.push(StatusLine {
                line_type: StatusLineType::OperationStatus,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::Suggestions) {
            result.push(StatusLine {
                line_type: StatusLineType::Suggestions,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::CompactionPercent) {
            result.push(StatusLine {
                line_type: StatusLineType::CompactionPercent,
                content: content.clone(),
            });
        }

        // Add custom lines (sorted by ID for consistency)
        let mut custom_lines: Vec<_> = lines
            .iter()
            .filter_map(|(k, v)| {
                if let StatusLineType::Custom(id) = k {
                    Some((id.clone(), v.clone()))
                } else {
                    None
                }
            })
            .collect();
        custom_lines.sort_by(|a, b| a.0.cmp(&b.0));

        for (id, content) in custom_lines {
            result.push(StatusLine {
                line_type: StatusLineType::Custom(id),
                content,
            });
        }

        result
    }

    /// Get the number of active status lines
    pub fn len(&self) -> usize {
        self.lines.read().unwrap().len()
    }

    /// Check if there are any status lines
    pub fn is_empty(&self) -> bool {
        self.lines.read().unwrap().is_empty()
    }

    /// Get one status line without exposing the internal map or its lock.
    pub fn get_line(&self, line_type: &StatusLineType) -> Option<String> {
        self.lines.read().unwrap().get(line_type).cloned()
    }

    /// Get status content as a string (for change detection)
    pub fn get_status(&self) -> String {
        let lines = self.get_lines();
        lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Get rendered status content while projecting one line somewhere else.
    pub fn get_status_without(&self, excluded: &StatusLineType) -> String {
        self.get_lines()
            .iter()
            .filter(|line| &line.line_type != excluded)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render the status bar as a multi-line string
    pub fn render(&self) -> String {
        let lines = self.get_lines();

        if lines.is_empty() {
            return String::new();
        }

        lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Update training stats line
    pub fn update_training_stats(
        &self,
        total_queries: usize,
        local_percentage: f64,
        quality_score: f64,
    ) {
        let content = format!(
            "Training: {} queries | Local: {:.0}% | Quality: {:.2}",
            total_queries,
            local_percentage * 100.0,
            quality_score
        );
        self.update_line(StatusLineType::TrainingStats, content);
    }

    /// Update download progress line
    pub fn update_download_progress(
        &self,
        model_name: impl Into<String>,
        percentage: f64,
        downloaded: u64,
        total: u64,
    ) {
        let model_name = model_name.into();
        let percentage = percentage.clamp(0.0, 1.0);
        let bar_width = 20;
        let filled = (percentage * bar_width as f64) as usize;
        let empty = bar_width - filled;

        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        let content = format!(
            "Downloading {}: {} {:.1}% ({}/{})",
            model_name,
            bar,
            percentage * 100.0,
            format_download_bytes(downloaded),
            format_download_bytes(total)
        );

        self.update_line(StatusLineType::DownloadProgress, content);
    }

    /// Remove the model download line after any terminal outcome.
    pub fn clear_download_progress(&self) {
        self.remove_line(&StatusLineType::DownloadProgress);
    }

    /// Update operation status line
    pub fn update_operation(&self, operation: impl Into<String>) {
        self.update_line(StatusLineType::OperationStatus, operation.into());
    }

    /// Clear operation status (shorthand)
    pub fn clear_operation(&self) {
        self.remove_line(&StatusLineType::OperationStatus);
    }

    /// Update live query statistics
    pub fn update_live_stats(
        &self,
        model: impl Into<String>,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: Option<u64>,
    ) {
        let model_name = model.into();

        let mut parts = vec![format!("Model: {}", model_name)];

        if let Some(input) = input_tokens {
            if let Some(output) = output_tokens {
                parts.push(format!("Tokens: {}→{}", input, output));
            } else {
                parts.push(format!("Input: {} tokens", input));
            }
        } else if let Some(output) = output_tokens {
            parts.push(format!("Output: {} tokens", output));
        }

        if let Some(latency) = latency_ms {
            let latency_sec = latency as f64 / 1000.0;
            parts.push(format!("Latency: {:.2}s", latency_sec));

            // Calculate tokens/sec if we have output tokens
            if let Some(output) = output_tokens {
                let tokens_per_sec = output as f64 / latency_sec;
                parts.push(format!("Speed: {:.1} tok/s", tokens_per_sec));
            }
        }

        let content = parts.join(" | ");
        self.update_line(StatusLineType::LiveStats, content);
    }

    /// Clear live stats (shorthand)
    pub fn clear_live_stats(&self) {
        self.remove_line(&StatusLineType::LiveStats);
    }

    /// Replace the session-cumulative usage line for this Brain. Tokens are
    /// always measured; the cost estimate appears only when price data exists.
    pub fn update_session_usage(
        &self,
        ledger: &crate::cli::usage::SessionUsageLedger,
        pricing: Option<&crate::cli::usage::ModelPricingTable>,
    ) {
        self.update_line(
            StatusLineType::SessionUsage,
            ledger.format_status_line(pricing),
        );
    }

    /// Replace the child activity aggregate in place. With no active children
    /// the bounded live status disappears instead of becoming session history.
    pub fn update_agent_activity(
        &self,
        active_children: usize,
        usage: &crate::cli::tui::ActivityUsage,
    ) {
        if active_children == 0 {
            self.remove_line(&StatusLineType::AgentActivity);
            return;
        }
        let input = usage
            .input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        let output = usage
            .output_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        let state = match usage.state {
            crate::cli::tui::ActivityUsageState::Complete => "complete",
            crate::cli::tui::ActivityUsageState::Partial => "partial",
            crate::cli::tui::ActivityUsageState::Unavailable => "unavailable",
        };
        self.update_line(
            StatusLineType::AgentActivity,
            format!(
                "Children: {active_children} active | Tokens: {input} input, {output} output | Usage: {state} ({}/{} attempts reported)",
                usage.reported_attempts, usage.started_attempts
            ),
        );
    }
}

fn format_download_bytes(bytes: u64) -> String {
    let megabytes = bytes as f64 / 1_000_000.0;
    // 999.95..1_000_000_000 would still round to "1000.0MB" at one decimal
    // place, which reads as a broken rollover next to a GB-formatted sibling
    // value; switch to GB formatting before that threshold instead of at the
    // raw byte cutoff.
    if megabytes >= 999.95 {
        format!("{:.1}GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{:.1}MB", megabytes)
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl super::tui::TuiStatusPort for StatusBar {
    fn status_without_session(&self) -> String {
        self.get_status_without(&StatusLineType::SessionLabel)
    }

    fn session_label(&self) -> Option<String> {
        self.get_line(&StatusLineType::SessionLabel)
    }

    fn update_agent_activity(&self, active_children: usize, usage: &super::tui::ActivityUsage) {
        StatusBar::update_agent_activity(self, active_children, usage);
    }

    fn set_operation(&self, operation: String) {
        self.update_operation(operation);
    }

    fn clear_operation(&self) {
        StatusBar::clear_operation(self);
    }
}

impl Clone for StatusBar {
    fn clone(&self) -> Self {
        Self {
            lines: Arc::clone(&self.lines),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_activity_status_formats_usage_and_clears_when_children_finish() {
        use crate::cli::tui::{ActivityUsage, ActivityUsageState};

        let status = StatusBar::new();
        status.update_agent_activity(
            2,
            &ActivityUsage {
                state: ActivityUsageState::Complete,
                input_tokens: Some(12),
                output_tokens: Some(7),
                reported_attempts: 1,
                started_attempts: 1,
            },
        );
        assert_eq!(
            status.get_line(&StatusLineType::AgentActivity).as_deref(),
            Some("Children: 2 active | Tokens: 12 input, 7 output | Usage: complete (1/1 attempts reported)")
        );
        status.update_agent_activity(
            2,
            &ActivityUsage {
                state: ActivityUsageState::Partial,
                input_tokens: Some(17),
                output_tokens: Some(7),
                reported_attempts: 2,
                started_attempts: 3,
            },
        );
        assert_eq!(
            status.get_line(&StatusLineType::AgentActivity).as_deref(),
            Some("Children: 2 active | Tokens: 17 input, 7 output | Usage: partial (2/3 attempts reported)")
        );
        status.update_agent_activity(
            1,
            &ActivityUsage {
                started_attempts: 1,
                ..ActivityUsage::default()
            },
        );
        assert_eq!(
            status.get_line(&StatusLineType::AgentActivity).as_deref(),
            Some("Children: 1 active | Tokens: unavailable input, unavailable output | Usage: unavailable (0/1 attempts reported)")
        );
        status.update_agent_activity(0, &ActivityUsage::default());
        assert_eq!(status.get_line(&StatusLineType::AgentActivity), None);
    }

    fn ledger_with(input_tokens: u32, output_tokens: u32) -> crate::cli::usage::SessionUsageLedger {
        let mut ledger = crate::cli::usage::SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(1500), Some(300));
        ledger.record_turn(
            "qwen-local",
            Some(input_tokens - 1500),
            Some(output_tokens - 300),
        );
        ledger
    }

    #[test]
    fn test_session_usage_line_renders_after_session_label() {
        let status = StatusBar::new();
        status.update_line(StatusLineType::MemoryContext, "Memory");
        status.update_session_usage(&ledger_with(182_000, 31_000), None);
        status.update_line(StatusLineType::SessionLabel, "Session");

        let lines = status.get_lines();
        assert_eq!(
            lines.len(),
            3,
            "session usage must render as its own ordered line; lines={lines:?}"
        );
        assert_eq!(lines[0].line_type, StatusLineType::SessionLabel);
        assert_eq!(lines[1].line_type, StatusLineType::SessionUsage);
        assert_eq!(
            lines[1].content, "this session: 182k in / 31k out",
            "the readout must match the issue's example format; lines={lines:?}"
        );
        assert_eq!(lines[2].line_type, StatusLineType::MemoryContext);
    }

    #[test]
    fn test_update_session_usage_estimates_cost_only_with_pricing() {
        let mut pricing = crate::cli::usage::ModelPricingTable::empty();
        pricing.insert("claude-sonnet-4-6", 3.0, 15.0);
        let mut ledger = crate::cli::usage::SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(182_000), Some(31_000));

        let status = StatusBar::new();
        status.update_session_usage(&ledger, Some(&pricing));
        let line = status
            .get_line(&StatusLineType::SessionUsage)
            .expect("session usage line must exist after an update");
        assert!(
            line.contains(", ~$1.01 est"),
            "priced burn must show a clearly-labeled estimate; line={line:?}"
        );

        let unpriced_status = StatusBar::new();
        unpriced_status.update_session_usage(&ledger, None);
        let line = unpriced_status
            .get_line(&StatusLineType::SessionUsage)
            .expect("session usage line must exist after an update");
        assert!(
            !line.contains('$'),
            "absent price data must not fabricate a cost; line={line:?}"
        );
    }

    #[test]
    fn test_basic_operations() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Test stats");
        status.update_line(StatusLineType::OperationStatus, "Test operation");

        assert_eq!(status.len(), 2);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(lines[1].line_type, StatusLineType::OperationStatus);
    }

    #[test]
    fn test_update_overwrites() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "First");
        status.update_line(StatusLineType::TrainingStats, "Second");

        assert_eq!(status.len(), 1);

        let lines = status.get_lines();
        assert_eq!(lines[0].content, "Second");
    }

    #[test]
    fn test_remove_line() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Test");
        assert_eq!(status.len(), 1);

        status.remove_line(&StatusLineType::TrainingStats);
        assert_eq!(status.len(), 0);
        assert!(status.is_empty());
    }

    #[test]
    fn test_line_ordering() {
        let status = StatusBar::new();

        // Add in random order
        status.update_line(StatusLineType::OperationStatus, "Operation");
        status.update_line(StatusLineType::TrainingStats, "Training");
        status.update_line(StatusLineType::DownloadProgress, "Download");

        let lines = status.get_lines();

        // Should be ordered: Training, Download, Operation
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(lines[1].line_type, StatusLineType::DownloadProgress);
        assert_eq!(lines[2].line_type, StatusLineType::OperationStatus);
    }

    #[test]
    fn test_status_line_ordering_with_session_and_memory() {
        let status = StatusBar::new();

        // Add in reverse order — LiveStats is suppressed (not rendered)
        status.update_line(StatusLineType::MemoryContext, "Memory");
        status.update_line(StatusLineType::SessionLabel, "Session");

        let lines = status.get_lines();

        // SessionLabel must be first, MemoryContext second
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_type, StatusLineType::SessionLabel);
        assert_eq!(lines[1].line_type, StatusLineType::MemoryContext);
    }

    #[test]
    fn semantic_and_canonical_brain_context_have_independent_slots() {
        let status = StatusBar::new();
        status.update_line(StatusLineType::BrainContextLine(0), "Brain turn");
        status.update_line(StatusLineType::ContextLine(0), "Semantic summary");
        status.update_line(StatusLineType::MemoryContext, "Recall status");

        status.remove_line(&StatusLineType::ContextLine(0));

        let lines = status.get_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_type, StatusLineType::MemoryContext);
        assert_eq!(lines[1].line_type, StatusLineType::BrainContextLine(0));
        assert_eq!(lines[1].content, "   └─ now: Brain turn");
    }

    #[test]
    fn brain_log_hides_duplicate_memtree_conversation_tree() {
        let status = StatusBar::new();
        status.update_line(StatusLineType::MemoryContext, "🧠 recalled 2");
        status.update_line(StatusLineType::ContextLine(0), "📋 semantic topic");
        status.update_line(StatusLineType::ContextLine(1), "   └─ now: semantic focus");
        status.update_line(StatusLineType::BrainContextLine(0), "💬 shammah: test");
        status.update_line(
            StatusLineType::BrainContextLine(1),
            "   └─ now: daemon: Test received successfully.",
        );

        let lines = status.get_lines();
        let contents: Vec<&str> = lines.iter().map(|line| line.content.as_str()).collect();
        assert_eq!(
            contents,
            [
                "🧠 recalled 2",
                "💬 shammah: test",
                "   └─ now: daemon: Test received successfully.",
            ],
            "MemTree 📋/now must not stack under the Brain 💬/now tree"
        );
        assert!(
            !lines
                .iter()
                .any(|line| matches!(line.line_type, StatusLineType::ContextLine(_))),
            "ContextLine must stay stored but not rendered while a Brain 💬/now tree is present"
        );
    }

    #[test]
    fn two_singleton_now_recaps_fold_into_one_tree() {
        let status = StatusBar::new();
        status.update_line(StatusLineType::MemoryContext, "🧠 recalled 2");
        status.update_line(
            StatusLineType::ContextLine(0),
            "   └─ now: I think Finch is unusually ambitious…",
        );
        status.update_line(
            StatusLineType::BrainContextLine(0),
            "   └─ now: shammah: hello",
        );

        let contents: Vec<String> = status
            .get_lines()
            .into_iter()
            .map(|line| line.content)
            .collect();
        assert_eq!(
            contents,
            vec![
                "🧠 recalled 2".to_string(),
                "📋 I think Finch is unusually ambitious…".to_string(),
                "   └─ now: shammah: hello".to_string(),
            ],
            "two singleton now: projectors must render as one recap tree"
        );
        assert_eq!(
            contents
                .iter()
                .filter(|line| line.contains("└─ now:"))
                .count(),
            1
        );
    }

    #[test]
    fn session_label_can_be_projected_out_of_the_status_body() {
        let status = StatusBar::new();
        status.update_line(StatusLineType::SessionLabel, "◆ brain: quiet-hill · runner");
        status.update_line(StatusLineType::MemoryContext, "🧠 recalled 2");

        assert_eq!(
            status.get_line(&StatusLineType::SessionLabel).as_deref(),
            Some("◆ brain: quiet-hill · runner")
        );
        assert_eq!(
            status.get_status_without(&StatusLineType::SessionLabel),
            "🧠 recalled 2"
        );
    }

    #[test]
    fn test_live_stats_not_rendered() {
        // LiveStats is suppressed — pushing it should not cause it to appear in get_lines()
        let status = StatusBar::new();
        status.update_live_stats("Qwen-3B", Some(100), Some(50), Some(1200));
        let lines = status.get_lines();
        assert!(
            lines
                .iter()
                .all(|l| l.line_type != StatusLineType::LiveStats),
            "LiveStats must not appear in rendered output"
        );
    }

    #[test]
    fn test_training_stats_format() {
        let status = StatusBar::new();

        status.update_training_stats(42, 0.38, 0.82);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].content,
            "Training: 42 queries | Local: 38% | Quality: 0.82"
        );
    }

    #[test]
    fn test_download_progress_format() {
        let status = StatusBar::new();

        status.update_download_progress("Qwen-2.5-3B", 0.80, 2_100_000_000, 2_600_000_000);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].content.contains("Downloading Qwen-2.5-3B"));
        assert!(lines[0].content.contains("80.0%"));
        assert!(lines[0].content.contains("2.1GB"));
        assert!(lines[0].content.contains("2.6GB"));
    }

    #[test]
    fn test_download_progress_keeps_early_bytes_visible() {
        let status = StatusBar::new();

        status.update_download_progress("Qwen-2.5-3B", 0.014, 30_277_128, 2_104_932_768);

        let content = &status.get_lines()[0].content;
        assert!(content.contains("1.4%"), "content={content:?}");
        assert!(content.contains("30.3MB/2.1GB"), "content={content:?}");
    }

    #[test]
    fn test_download_progress_near_one_gigabyte_rolls_over_instead_of_reading_1000_0mb() {
        // 999_950_000..1_000_000_000 bytes is < the raw GB byte cutoff, but
        // formatting it in MB at one decimal place rounds up to "1000.0MB" —
        // a value that reads as a broken rollover next to a GB-formatted
        // sibling. It must switch to GB formatting before that point.
        let status = StatusBar::new();

        status.update_download_progress("Qwen-2.5-3B", 0.9999, 999_960_000, 1_000_000_000);

        let content = &status.get_lines()[0].content;
        assert!(
            !content.contains("1000.0MB"),
            "content={content:?}: must not display a value that rounds to a fake 1000MB"
        );
        assert!(content.contains("1.0GB/1.0GB"), "content={content:?}");
    }

    #[test]
    fn test_render() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Line 1");
        status.update_line(StatusLineType::OperationStatus, "Line 2");

        let rendered = status.render();
        assert_eq!(rendered, "Line 1\nLine 2");
    }

    #[test]
    fn test_status_line_ordering_conversation_before_live_stats() {
        let status = StatusBar::new();

        // LiveStats is suppressed — only the non-Live lines appear
        status.update_line(StatusLineType::ConversationFocus, "Focus");
        status.update_line(StatusLineType::ConversationTopic, "Topic");
        status.update_line(StatusLineType::MemoryContext, "Memory");
        status.update_line(StatusLineType::SessionLabel, "Session");

        let lines = status.get_lines();

        assert_eq!(lines[0].line_type, StatusLineType::SessionLabel);
        assert_eq!(lines[1].line_type, StatusLineType::MemoryContext);
        assert_eq!(lines[2].line_type, StatusLineType::ConversationTopic);
        assert_eq!(lines[3].line_type, StatusLineType::ConversationFocus);
        assert_eq!(lines.len(), 4, "LiveStats must not appear");
    }

    #[test]
    fn test_custom_lines() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::Custom("test1".to_string()), "Custom 1");
        status.update_line(StatusLineType::Custom("test2".to_string()), "Custom 2");
        status.update_line(StatusLineType::TrainingStats, "Training");

        let lines = status.get_lines();

        // Training should be first, then custom lines (sorted)
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(
            lines[1].line_type,
            StatusLineType::Custom("test1".to_string())
        );
        assert_eq!(
            lines[2].line_type,
            StatusLineType::Custom("test2".to_string())
        );
    }
}
