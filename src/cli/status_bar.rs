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
use std::time::Instant;

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
    /// Byte/time samples for the in-progress model download, used to derive
    /// a smoothed transfer rate and ETA. Keyed by model name alongside the
    /// tracker so a different download (e.g. the background embedding-model
    /// fetch replacing the chat-model fetch on the same status line) starts
    /// a fresh rate estimate instead of blending unrelated transfers.
    download_rate: RwLock<Option<(String, DownloadRateTracker)>>,
}

impl StatusBar {
    /// Create a new StatusBar
    pub fn new() -> Self {
        Self {
            lines: Arc::new(RwLock::new(HashMap::new())),
            download_rate: RwLock::new(None),
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
        self.update_download_progress_at(model_name, percentage, downloaded, total, Instant::now());
    }

    /// Same as [`Self::update_download_progress`] but with the sample
    /// timestamp taken explicitly rather than from the clock. Split out so
    /// tests can drive the rate/ETA calculation with synthetic timestamps
    /// instead of sleeping for real elapsed time.
    fn update_download_progress_at(
        &self,
        model_name: impl Into<String>,
        percentage: f64,
        downloaded: u64,
        total: u64,
        at: Instant,
    ) {
        let model_name = model_name.into();
        let rate = self.observe_download_rate(&model_name, downloaded, at);
        let percentage = percentage.clamp(0.0, 1.0);
        let bar_width = 20;
        let filled = (percentage * bar_width as f64) as usize;
        let empty = bar_width - filled;

        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        let eta_suffix = rate
            .and_then(|bytes_per_sec| {
                format_eta_remaining(total.saturating_sub(downloaded), bytes_per_sec)
            })
            .map(|eta| format!(" · {eta}"))
            .unwrap_or_default();

        let content = format!(
            "Downloading {}: {} {:.1}% ({}/{}){}",
            model_name,
            bar,
            percentage * 100.0,
            format_download_bytes(downloaded),
            format_download_bytes(total),
            eta_suffix
        );

        self.update_line(StatusLineType::DownloadProgress, content);
    }

    /// Record a byte-count sample for the in-progress download and return
    /// the current smoothed transfer rate, if one has been established yet.
    /// A different `model_name` than the last observed sample resets the
    /// tracker, since a new download's byte count and clock have no
    /// relationship to the previous one's.
    fn observe_download_rate(&self, model_name: &str, downloaded: u64, at: Instant) -> Option<f64> {
        let mut state = self.download_rate.write().unwrap();
        let tracker = match state.as_mut() {
            Some((name, tracker)) if name == model_name => tracker,
            _ => {
                *state = Some((model_name.to_string(), DownloadRateTracker::new()));
                &mut state.as_mut().unwrap().1
            }
        };
        tracker.observe(at, downloaded)
    }

    /// Remove the model download line after any terminal outcome.
    pub fn clear_download_progress(&self) {
        self.remove_line(&StatusLineType::DownloadProgress);
        *self.download_rate.write().unwrap() = None;
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

/// Exponential-moving-average weight applied to each newly observed
/// instantaneous rate. Network throughput is noisy tick-to-tick; a low
/// weight favors the established trend so the displayed ETA does not
/// flicker between successive polls (the chat-model monitor polls every
/// 250ms, the background embedding download every 150ms).
const DOWNLOAD_RATE_EMA_ALPHA: f64 = 0.3;

/// Derives a smoothed download rate from successive timestamped
/// byte-count samples. The first sample only seeds state — there is no
/// prior point to compute a rate from — so it reports no rate; each
/// sample after that blends the newly observed instantaneous rate into an
/// exponential moving average.
#[derive(Debug, Clone, Default)]
struct DownloadRateTracker {
    last_sample: Option<(Instant, u64)>,
    smoothed_bytes_per_sec: Option<f64>,
}

impl DownloadRateTracker {
    fn new() -> Self {
        Self::default()
    }

    /// Record a `(timestamp, total-bytes-downloaded-so-far)` sample and
    /// return the current smoothed bytes/sec, or `None` if no valid rate
    /// has been established yet (first sample, or every sample so far has
    /// had non-advancing time or bytes, e.g. a stalled or duplicate poll).
    fn observe(&mut self, at: Instant, downloaded: u64) -> Option<f64> {
        if let Some((last_at, last_downloaded)) = self.last_sample {
            let elapsed = at.saturating_duration_since(last_at).as_secs_f64();
            if elapsed > 0.0 && downloaded > last_downloaded {
                let instantaneous = (downloaded - last_downloaded) as f64 / elapsed;
                self.smoothed_bytes_per_sec = Some(match self.smoothed_bytes_per_sec {
                    Some(prev) => {
                        DOWNLOAD_RATE_EMA_ALPHA * instantaneous
                            + (1.0 - DOWNLOAD_RATE_EMA_ALPHA) * prev
                    }
                    None => instantaneous,
                });
            }
        }
        self.last_sample = Some((at, downloaded));
        self.smoothed_bytes_per_sec
    }
}

/// Format remaining time as "~Xm Ys remaining" (or "~Xh Ym remaining" /
/// "~Xs remaining"), or `None` when the rate cannot produce a sane
/// estimate (zero, negative, non-finite).
fn format_eta_remaining(remaining_bytes: u64, bytes_per_sec: f64) -> Option<String> {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return None;
    }
    let seconds_remaining = remaining_bytes as f64 / bytes_per_sec;
    if !seconds_remaining.is_finite() || seconds_remaining < 0.0 {
        return None;
    }

    let total_seconds = seconds_remaining.round() as u64;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    Some(if hours > 0 {
        format!("~{hours}h {minutes}m remaining")
    } else if minutes > 0 {
        format!("~{minutes}m {secs}s remaining")
    } else {
        format!("~{secs}s remaining")
    })
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
            // Intentionally NOT shared with the source: each clone tracks
            // download rate independently, matching `download_rate`'s own
            // (non-`Arc`) field type. Every caller that drives download
            // progress (the local-model monitor task, the background
            // embedding-model download) clones a `StatusBar` once up front
            // and then reuses that single clone for every sample in its
            // loop, so the tracker still sees a consistent sample sequence;
            // it just isn't visible from sibling clones, unlike `lines`.
            download_rate: RwLock::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    // --- DownloadRateTracker: pure rate calculation from synthetic samples ---

    #[test]
    fn test_download_rate_tracker_first_sample_reports_no_rate() {
        let mut tracker = DownloadRateTracker::new();
        let t0 = Instant::now();

        let rate = tracker.observe(t0, 1_000_000);

        assert_eq!(
            rate, None,
            "first sample has no prior data point to derive a rate from, got {rate:?}"
        );
    }

    #[test]
    fn test_download_rate_tracker_second_sample_computes_instantaneous_rate() {
        let mut tracker = DownloadRateTracker::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(2);

        tracker.observe(t0, 1_000_000);
        let rate = tracker.observe(t1, 3_000_000);

        // 2_000_000 bytes over 2 seconds = 1_000_000 bytes/sec; with only one
        // prior instantaneous sample the EMA has nothing to blend against yet.
        assert_eq!(
            rate,
            Some(1_000_000.0),
            "expected the raw instantaneous rate on the first blended sample, got {rate:?}"
        );
    }

    #[test]
    fn test_download_rate_tracker_smooths_across_samples() {
        let mut tracker = DownloadRateTracker::new();
        let t0 = Instant::now();

        // 1 MB/s, then a noisy 9 MB/s tick.
        tracker.observe(t0, 0);
        tracker.observe(t0 + Duration::from_secs(1), 1_000_000);
        let rate = tracker
            .observe(t0 + Duration::from_secs(2), 10_000_000)
            .expect("third sample should produce a smoothed rate");

        // EMA(alpha=0.3): 0.3 * 9_000_000 + 0.7 * 1_000_000 = 3_400_000.
        // Must land strictly between the two instantaneous rates -- a
        // noisy tick should not swing the estimate all the way to 9MB/s,
        // which is what a raw two-sample rate would report.
        assert!(
            rate > 1_000_000.0 && rate < 9_000_000.0,
            "smoothed rate {rate} should sit between the prior (1e6) and the noisy instantaneous \
             rate (9e6), not track the latest tick directly"
        );
        let expected = 3_400_000.0;
        assert!(
            (rate - expected).abs() < 1.0,
            "expected EMA(alpha=0.3) of prior 1e6 and instantaneous 9e6 to be {expected}, got {rate}"
        );
    }

    #[test]
    fn test_download_rate_tracker_stalled_tick_keeps_previous_rate() {
        let mut tracker = DownloadRateTracker::new();
        let t0 = Instant::now();

        tracker.observe(t0, 0);
        let established = tracker
            .observe(t0 + Duration::from_secs(1), 1_000_000)
            .expect("second sample should establish a rate");

        // Same byte count, same timestamp as the prior sample: a duplicate
        // or stalled poll must not divide by zero or erase the rate.
        let rate = tracker.observe(t0 + Duration::from_secs(1), 1_000_000);

        assert_eq!(
            rate,
            Some(established),
            "a non-advancing sample (zero elapsed time and zero new bytes) must keep the last \
             established rate ({established}) rather than reporting None or a bogus value, got {rate:?}"
        );
    }

    #[test]
    fn test_download_rate_tracker_non_advancing_bytes_does_not_panic_or_go_negative() {
        let mut tracker = DownloadRateTracker::new();
        let t0 = Instant::now();

        tracker.observe(t0, 5_000_000);
        // Downloaded count went backwards (e.g. a restarted transfer) with
        // time still advancing; must not underflow `downloaded - last_downloaded`.
        let rate = tracker.observe(t0 + Duration::from_secs(1), 1_000_000);

        assert_eq!(
            rate, None,
            "a byte count that regresses must not produce a rate (no established rate existed \
             yet), got {rate:?}"
        );
    }

    // --- format_eta_remaining: pure duration formatting ---

    #[test]
    fn test_format_eta_remaining_formats_minutes_and_seconds() {
        // 5.8GB total, 550.4MB downloaded => ~5.25GB remaining at 40MB/s.
        let remaining_bytes = 5_249_600_000u64;
        let bytes_per_sec = 40_000_000.0;

        let eta = format_eta_remaining(remaining_bytes, bytes_per_sec);

        assert_eq!(
            eta,
            Some("~2m 11s remaining".to_string()),
            "5,249,600,000 bytes at 40,000,000 bytes/sec = 131.24s ~= 2m 11s, got {eta:?}"
        );
    }

    #[test]
    fn test_format_eta_remaining_formats_hours() {
        let eta = format_eta_remaining(7_200_000_000, 1_000_000.0);

        assert_eq!(
            eta,
            Some("~2h 0m remaining".to_string()),
            "7,200,000,000 bytes at 1,000,000 bytes/sec = 7200s = 2h exactly, got {eta:?}"
        );
    }

    #[test]
    fn test_format_eta_remaining_formats_seconds_only_under_a_minute() {
        let eta = format_eta_remaining(45_000_000, 10_000_000.0);

        assert_eq!(
            eta,
            Some("~5s remaining".to_string()),
            "45,000,000 bytes at 10,000,000 bytes/sec = 4.5s, rounds to 5s, got {eta:?}"
        );
    }

    #[test]
    fn test_format_eta_remaining_none_for_zero_or_negative_rate() {
        assert_eq!(
            format_eta_remaining(1_000_000, 0.0),
            None,
            "zero rate must not divide-by-zero into an infinite ETA"
        );
        assert_eq!(
            format_eta_remaining(1_000_000, -5.0),
            None,
            "negative rate must not report a nonsensical negative-time ETA"
        );
    }

    #[test]
    fn test_format_eta_remaining_none_for_non_finite_rate() {
        assert_eq!(
            format_eta_remaining(1_000_000, f64::NAN),
            None,
            "NaN rate must not propagate into the displayed ETA"
        );
        assert_eq!(
            format_eta_remaining(1_000_000, f64::INFINITY),
            None,
            "infinite rate must not propagate into the displayed ETA"
        );
    }

    // --- production boundary: the assembled status line ---

    #[test]
    fn test_download_progress_first_sample_shows_no_eta() {
        let status = StatusBar::new();
        let t0 = Instant::now();

        status.update_download_progress_at("Gemma 2 9b", 0.096, 550_400_000, 5_800_000_000, t0);

        let content = &status.get_lines()[0].content;
        assert!(
            !content.contains("remaining"),
            "the first sample has no prior data point to compute a rate from, so no ETA should \
             appear yet: content={content:?}"
        );
    }

    #[test]
    fn test_download_progress_appends_eta_after_second_sample() {
        let status = StatusBar::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);

        // 550.4MB, then +40MB one second later => 40MB/s.
        status.update_download_progress_at("Gemma 2 9b", 0.096, 550_400_000, 5_800_000_000, t0);
        status.update_download_progress_at("Gemma 2 9b", 0.103, 590_400_000, 5_800_000_000, t1);

        let content = status.get_lines()[0].content.clone();
        let byte_parenthetical = format!(
            "({}/{})",
            format_download_bytes(590_400_000),
            format_download_bytes(5_800_000_000)
        );
        let eta_pos = content
            .find("remaining")
            .unwrap_or_else(|| panic!("expected an ETA once a rate is known: content={content:?}"));
        let bytes_pos = content.find(&byte_parenthetical).unwrap_or_else(|| {
            panic!("expected the byte-count parenthetical {byte_parenthetical:?} in content={content:?}")
        });
        assert!(
            eta_pos > bytes_pos,
            "the ETA must appear after the byte-count parenthetical, not before it: \
             content={content:?}"
        );
        assert!(
            content.contains(" · "),
            "expected the ETA to be separated from the byte-count parenthetical with \" · \": \
             content={content:?}"
        );
    }

    #[test]
    fn test_download_progress_eta_resets_when_model_name_changes() {
        let status = StatusBar::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);

        status.update_download_progress_at("Gemma 2 9b", 0.5, 1_000_000, 2_000_000, t0);
        // A different download (e.g. the background embedding-model fetch)
        // reuses the same status line; its first sample must not inherit
        // the previous download's rate.
        status.update_download_progress_at("memory embeddings", 0.01, 10_000, 1_000_000, t1);

        let content = &status.get_lines()[0].content;
        assert!(
            !content.contains("remaining"),
            "a new download's first sample must show no ETA even though a prior, unrelated \
             download had already established a rate: content={content:?}"
        );
    }

    #[test]
    fn test_download_progress_clear_resets_rate_tracker() {
        let status = StatusBar::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        let t2 = t1 + Duration::from_secs(1);

        status.update_download_progress_at("Gemma 2 9b", 0.5, 1_000_000, 2_000_000, t0);
        status.update_download_progress_at("Gemma 2 9b", 0.6, 1_200_000, 2_000_000, t1);
        status.clear_download_progress();

        // Same model name as before the clear: without a reset this would
        // be treated as a continuing sample and could show a rate derived
        // from a stale timestamp.
        status.update_download_progress_at("Gemma 2 9b", 0.05, 100_000, 2_000_000, t2);

        let content = &status.get_lines()[0].content;
        assert!(
            !content.contains("remaining"),
            "clearing the download line must reset the rate tracker so a restarted download's \
             first sample shows no ETA: content={content:?}"
        );
    }

    #[test]
    fn test_clone_shares_status_lines_but_tracks_download_rate_independently() {
        // Regression test for a StatusBar::clone() that forgot to initialize
        // the `download_rate` field it added, which failed to compile
        // (E0063: missing field `download_rate` in initializer of
        // `StatusBar`). This exercises the intended semantics now that the
        // field is present: `lines` is `Arc`-shared (as it always was,
        // matching every caller that clones a StatusBar to hand to a
        // spawned monitor task and still expects the original to observe
        // the same rendered line), while `download_rate` is deliberately
        // NOT shared, so each clone starts its own rate estimate.
        let status = StatusBar::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);

        status.update_download_progress_at("Gemma 2 9b", 0.5, 1_000_000, 2_000_000, t0);
        status.update_download_progress_at("Gemma 2 9b", 0.6, 1_200_000, 2_000_000, t1);
        let established_content = status.get_lines()[0].content.clone();
        assert!(
            established_content.contains("remaining"),
            "the source StatusBar should have an established rate before cloning: \
             content={established_content:?}"
        );

        let cloned = status.clone();
        let cloned_before_update = cloned.get_lines()[0].content.clone();
        assert_eq!(
            cloned_before_update, established_content,
            "lines must stay Arc-shared across clone(), so the clone should see the same \
             rendered line the source already produced, got {cloned_before_update:?}"
        );

        // Continuing the same byte progression through the clone: since the
        // clone's `download_rate` tracker starts empty (not shared with the
        // source), this sample is its "first sample" and must show no ETA,
        // even though the underlying transfer has been running for two
        // samples already.
        let t2 = t1 + Duration::from_secs(1);
        cloned.update_download_progress_at("Gemma 2 9b", 0.7, 1_400_000, 2_000_000, t2);
        let after_clone_update = cloned.get_lines()[0].content.clone();
        assert!(
            !after_clone_update.contains("remaining"),
            "a clone's independent download_rate tracker has seen only one sample so far, so \
             the shared line it just wrote must show no ETA yet: content={after_clone_update:?}"
        );

        // Because `lines` is shared, the source now observes the clone's
        // write too (last writer wins on the shared map).
        let source_after_clone_update = status.get_lines()[0].content.clone();
        assert_eq!(
            source_after_clone_update, after_clone_update,
            "the source must observe the clone's write to the shared `lines` map, got \
             {source_after_clone_update:?}"
        );
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
