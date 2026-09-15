# planning — public interface

Generated from [`src/planning/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/planning/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Result of a convergence check between two successive plan iterations
pub enum ConvergenceResult { Stable, ScopeRunaway, Continuing }
/// A single critique concern from one persona
pub struct CritiqueItem { … }
impl CritiqueItem {
    /// Build a CritiqueItem, computing derived fields automatically.
    pub fn new(persona: impl Into<String>, concern: impl Into<String>, step_ref: Option<usize>, severity: u8, confidence: u8) -> Self;
}
/// Configuration for the IMPCPD loop
pub struct ImpcpdConfig { … }
/// One complete plan iteration: draft text + critique received + optional user feedback
pub struct PlanIteration { … }
/// The IMPCPD plan loop.
pub struct PlanLoop { … }
impl PlanLoop {
    /// Run the full IMPCPD loop for a planning task.
    pub async fn run(&self, task: &str, tui: Arc<Mutex<TuiRenderer>>) -> Result<PlanResult>;
    pub fn new(generator: Arc<dyn Generator>, output_manager: Arc<OutputManager>, config: ImpcpdConfig) -> Self;
}
/// Result returned from `PlanLoop::run()`
pub enum PlanResult { Converged, UserApproved, Cancelled, IterationCap }
impl PlanResult {
    /// Return the last plan text, regardless of how the loop ended.
    pub fn final_plan(&self) -> Option<&str>;
}
/// Internal user feedback result from the steering dialog
pub enum UserFeedback { Approve, Cancel, Continue }
```

## Functions

```rust
/// Return the active critique personas for the given plan text.
pub fn select_active_personas(plan_text: &str) -> Vec<&'static str> { … }
```

## Constants

```rust
/// The IMPCPD runtime methodology spec, embedded at compile time.
pub const IMPCPD_METHODOLOGY: &str = include_str!("impcpd_methodology.md");
```
