# run — public interface

Generated from [`crates/finch-brain/src/run/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-brain/src/run/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub struct BrainRun { … }
pub struct BrainRunCancellationReservation { … }
pub enum BrainRunKind { Interactive, Speculative, Scheduled, Subagent, Maintenance }
pub enum BrainRunStatus { QueuedForEnvironment, Running, AwaitingApproval, Completed, Failed, Cancelled, Interrupted }
impl BrainRunStatus {
    /// Human status label for TUI, raw mode, and screen-reader text.
    pub fn human_label(self) -> &'static str;
    pub fn is_terminal(self) -> bool;
}
pub struct BrainRunnerHandoff { … }
pub struct BrainRunnerLease { … }
pub struct DisconnectTerminalizationIntent { … }
pub struct RunId(pub uuid::Uuid);
pub struct RunnerHandoffId(pub uuid::Uuid);
pub struct RunnerLeaseId(pub uuid::Uuid);
```

## Functions

```rust
pub fn clear_disconnect_intent(root: Option<&Path>, name: &str, run_id: RunId) -> Result<()> { … }
pub fn disconnect_intent_path(root: Option<&Path>, name: &str, run_id: RunId) -> Option<PathBuf> { … }
pub fn persist_disconnect_intent(root: Option<&Path>, name: &str, intent: &DisconnectTerminalizationIntent) -> Result<()> { … }
pub fn read_disconnect_intents(root: Option<&Path>, name: &str) -> Result<HashMap<RunId, DisconnectTerminalizationIntent>> { … }
pub fn sorted_runs(runs: &HashMap<RunId, BrainRun>) -> Vec<BrainRun> { … }
/// Reject any transition that would leave a terminal run, or skip an allowed step.
pub fn validate_run_transition(from: BrainRunStatus, to: BrainRunStatus) -> Result<()> { … }
```
