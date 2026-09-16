// Session-cumulative token accounting per Brain.
//
// Accumulates the input/output token counts that generators already report
// through `StreamChunk::Usage` / `ResponseMetadata` (funnelled to the event
// loop as `ReplEvent::StatsUpdate`). The ledger is a local observation of
// provider-reported usage for the current Brain session: it is neither a
// billing statement nor a provider allowance snapshot (see the quota dialog
// work), and it never contacts a provider.
//
// Persistence: one small JSON file per Brain under `~/.finch/usage/`, written
// after every recorded turn and on shutdown, so attach/resume restores the
// running total. Reset is explicit only (`/usage reset`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Cumulative usage attributed to one model within a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub turns: u64,
}

/// Session-cumulative token accounting for one Brain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsageLedger {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub turns: u64,
    pub per_model: BTreeMap<String, ModelUsage>,
}

/// Optional price data for the clearly-labeled cost estimate.
///
/// Rates are USD per one million tokens, keyed by exact model name. Nothing
/// populates this table yet: the model catalog and provider entries carry no
/// price fields today, so the CLI renders tokens only. When a price source
/// lands in the catalog, insert its rows here and the cost estimate appears.
#[derive(Debug, Clone, Default)]
pub struct ModelPricingTable {
    entries: BTreeMap<String, (f64, f64)>,
}

impl ModelPricingTable {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, model: &str, input_usd_per_mtok: f64, output_usd_per_mtok: f64) {
        self.entries
            .insert(model.to_string(), (input_usd_per_mtok, output_usd_per_mtok));
    }

    pub fn price_for(&self, model: &str) -> Option<(f64, f64)> {
        self.entries.get(model).copied()
    }
}

impl SessionUsageLedger {
    /// Record one completed provider response. Turns without any reported
    /// token counts change nothing: the ledger only accumulates measured
    /// usage, never estimates.
    pub fn record_turn(
        &mut self,
        model: &str,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    ) {
        if input_tokens.is_none() && output_tokens.is_none() {
            return;
        }
        let input = u64::from(input_tokens.unwrap_or(0));
        let output = u64::from(output_tokens.unwrap_or(0));
        let entry = self.per_model.entry(model.to_string()).or_default();
        entry.input_tokens += input;
        entry.output_tokens += output;
        entry.turns += 1;
        self.input_tokens += input;
        self.output_tokens += output;
        self.turns += 1;
    }

    /// Whether nothing has been recorded at all.
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && self.input_tokens == 0 && self.output_tokens == 0
    }

    /// Explicit reset. Persistence restores totals across attach/resume, so
    /// this is the only way the running total returns to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Estimated cost of the recorded burn under `pricing`, in USD. Models
    /// without a price row are skipped, so the result undercounts rather than
    /// invents a rate. Callers must label it as an estimate.
    pub fn estimated_cost_usd(&self, pricing: &ModelPricingTable) -> f64 {
        self.per_model
            .iter()
            .filter_map(|(model, usage)| {
                let (input_rate, output_rate) = pricing.price_for(model)?;
                Some(
                    usage.input_tokens as f64 * input_rate / 1_000_000.0
                        + usage.output_tokens as f64 * output_rate / 1_000_000.0,
                )
            })
            .sum()
    }

    /// The status-line readout: tokens always; a clearly-labeled `~$` estimate
    /// only when price data exists. With no pricing (the state today) the line
    /// stays tokens-only.
    pub fn format_status_line(&self, pricing: Option<&ModelPricingTable>) -> String {
        let mut line = format!(
            "this session: {} in / {} out",
            format_token_count(self.input_tokens),
            format_token_count(self.output_tokens)
        );
        if let Some(pricing) = pricing {
            if !pricing.is_empty() {
                let cost = self.estimated_cost_usd(pricing);
                if cost > 0.0 {
                    line.push_str(&format!(", ~${cost:.2} est"));
                }
            }
        }
        line
    }

    /// Multi-line scrollback detail for `/usage`.
    pub fn format_session_detail(&self, pricing: Option<&ModelPricingTable>) -> String {
        if self.is_empty() {
            return "No usage recorded this session yet.".to_string();
        }
        let mut lines = vec![
            "Session usage (local observation of provider-reported tokens; not a billing statement):"
                .to_string(),
            format!(
                "  total: {} in / {} out across {} response{}",
                format_token_count(self.input_tokens),
                format_token_count(self.output_tokens),
                self.turns,
                if self.turns == 1 { "" } else { "s" }
            ),
        ];
        for (model, usage) in &self.per_model {
            lines.push(format!(
                "  {model}: {} in / {} out ({} response{})",
                format_token_count(usage.input_tokens),
                format_token_count(usage.output_tokens),
                usage.turns,
                if usage.turns == 1 { "" } else { "s" }
            ));
        }
        if let Some(pricing) = pricing {
            if !pricing.is_empty() {
                lines.push(format!(
                    "  estimated cost: ~${:.2} est (priced models only; unpriced models are skipped)",
                    self.estimated_cost_usd(pricing)
                ));
            }
        }
        lines.push("Reset with /usage reset.".to_string());
        lines.join("\n")
    }

    /// Load the ledger persisted for this Brain. `Ok(None)` when no file
    /// exists yet; malformed content is an error, never silently discarded.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).context(format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse session usage from {}", path.display()))
            .map(Some)
    }

    /// Persist the ledger, replacing the file atomically so a crash mid-write
    /// cannot leave a torn checkpoint behind.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("failed to serialize usage")?;
        let staging = path.with_extension("json.tmp");
        std::fs::write(&staging, json)
            .with_context(|| format!("failed to write {}", staging.display()))?;
        std::fs::rename(&staging, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }
}

/// Human token counts: 823, 3.5k, 182k, 1.2M.
pub(crate) fn format_token_count(tokens: u64) -> String {
    fn scaled(value: f64, suffix: &str) -> String {
        let text = format!("{value:.1}");
        let text = text.trim_end_matches('0').trim_end_matches('.');
        format!("{text}{suffix}")
    }
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        scaled(tokens as f64 / 1_000.0, "k")
    } else {
        scaled(tokens as f64 / 1_000_000.0, "M")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_usage_ledger_accumulates_reported_usage_across_turns() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(1500), Some(300));
        ledger.record_turn("qwen-local", Some(2000), Some(500));
        assert_eq!(
            ledger.input_tokens, 3500,
            "session input must be the sum of reported per-turn inputs; ledger={ledger:?}"
        );
        assert_eq!(
            ledger.output_tokens, 800,
            "session output must be the sum of reported per-turn outputs; ledger={ledger:?}"
        );
        assert_eq!(
            ledger.turns, 2,
            "every turn with reported usage must count once; ledger={ledger:?}"
        );
    }

    #[test]
    fn test_session_usage_ledger_ignores_unreported_usage() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", None, None);
        assert!(
            ledger.is_empty(),
            "a turn with no reported token counts must not fabricate usage; ledger={ledger:?}"
        );
        ledger.record_turn("claude-sonnet-4-6", Some(1500), None);
        assert_eq!(
            (ledger.input_tokens, ledger.output_tokens, ledger.turns),
            (1500, 0, 1),
            "partial reporting must accumulate the reported side only; ledger={ledger:?}"
        );
    }

    #[test]
    fn test_session_usage_ledger_tracks_per_model_totals() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(1500), Some(300));
        ledger.record_turn("claude-sonnet-4-6", Some(500), Some(100));
        ledger.record_turn("qwen-local", Some(2000), Some(500));
        let claude = ledger
            .per_model
            .get("claude-sonnet-4-6")
            .expect("model row");
        assert_eq!(
            (claude.input_tokens, claude.output_tokens, claude.turns),
            (2000, 400, 2),
            "per-model totals must accumulate independently; ledger={ledger:?}"
        );
        let qwen = ledger.per_model.get("qwen-local").expect("model row");
        assert_eq!(
            (qwen.input_tokens, qwen.output_tokens, qwen.turns),
            (2000, 500, 1),
            "per-model totals must accumulate independently; ledger={ledger:?}"
        );
    }

    #[test]
    fn test_session_usage_ledger_reset_clears_all_totals() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(1500), Some(300));
        ledger.reset();
        assert_eq!(
            ledger,
            SessionUsageLedger::default(),
            "reset must return the ledger to its empty state; ledger={ledger:?}"
        );
    }

    #[test]
    fn test_session_usage_persistence_round_trip_preserves_totals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.usage.json");
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(182_000), Some(31_000));

        ledger
            .save(&path)
            .expect("saving a valid ledger must succeed");
        let restored = SessionUsageLedger::load(&path)
            .expect("loading a saved ledger must not error")
            .expect("a saved ledger must reload as Some");
        assert_eq!(
            restored, ledger,
            "attach/resume must restore the exact running total; saved={ledger:?} restored={restored:?}"
        );

        assert_eq!(
            SessionUsageLedger::load(&dir.path().join("missing.usage.json"))
                .expect("a missing file is not an error"),
            None,
            "a Brain with no checkpoint yet must start from an empty ledger"
        );
    }

    #[test]
    fn test_session_usage_status_line_shows_tokens_only_without_pricing() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(182_000), Some(31_000));
        let line = ledger.format_status_line(None);
        assert_eq!(
            line, "this session: 182k in / 31k out",
            "without price data the readout must be tokens only; line={line:?}"
        );
        let line_with_empty_table = ledger.format_status_line(Some(&ModelPricingTable::empty()));
        assert_eq!(
            line_with_empty_table, "this session: 182k in / 31k out",
            "an empty pricing table must behave like no pricing; line={line_with_empty_table:?}"
        );
    }

    #[test]
    fn test_session_usage_status_line_shows_estimated_cost_only_when_priced() {
        let mut pricing = ModelPricingTable::empty();
        pricing.insert("claude-sonnet-4-6", 3.0, 15.0);
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(182_000), Some(31_000));
        // 182000*3/1e6 + 31000*15/1e6 = 0.546 + 0.465 = 1.011
        let line = ledger.format_status_line(Some(&pricing));
        assert!(
            line.contains(", ~$1.01 est"),
            "priced burn must show a clearly-labeled estimate; line={line:?}"
        );

        let mut unpriced = SessionUsageLedger::default();
        unpriced.record_turn("qwen-local", Some(182_000), Some(31_000));
        let line = unpriced.format_status_line(Some(&pricing));
        assert_eq!(
            line, "this session: 182k in / 31k out",
            "models without a price row must not produce a zero-dollar fake estimate; line={line:?}"
        );
    }

    #[test]
    fn test_format_token_count_scales_units() {
        assert_eq!(format_token_count(823), "823");
        assert_eq!(format_token_count(3_500), "3.5k");
        assert_eq!(format_token_count(182_000), "182k");
        assert_eq!(format_token_count(1_200_000), "1.2M");
    }

    #[test]
    fn test_session_usage_detail_reports_totals_and_models() {
        let mut ledger = SessionUsageLedger::default();
        ledger.record_turn("claude-sonnet-4-6", Some(1500), Some(300));
        let detail = ledger.format_session_detail(None);
        assert!(
            detail.contains("total: 1.5k in / 300 out across 1 response"),
            "detail must state exact totals; detail={detail:?}"
        );
        assert!(
            detail.contains("claude-sonnet-4-6: 1.5k in / 300 out"),
            "detail must break usage down per model; detail={detail:?}"
        );
        assert!(
            !detail.contains('$'),
            "no pricing source exists, so the detail must not show a cost; detail={detail:?}"
        );

        let empty = SessionUsageLedger::default().format_session_detail(None);
        assert_eq!(
            empty, "No usage recorded this session yet.",
            "an empty ledger must say so instead of printing zero rows; detail={empty:?}"
        );
    }
}
