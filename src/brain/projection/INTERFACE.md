# brain::projection — public interface

Generated from [`src/brain/projection/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/brain/projection/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// The one machine/workspace boundary in which a brain may cause effects.
pub struct BrainEnvironment { … }
/// One live subagent run, as projected from the event log without hydrating.
pub struct BrainListAgent { … }
/// One currently connected participant, as projected from the event log without hydrating the Brain.
pub struct BrainListAttachment { … }
/// Directory-and-journal facts about one named Brain, gathered without replaying the reducer, opening effect-audit databases, or creating files.
pub struct BrainListSummary { … }
pub struct BrainSnapshot { … }
impl BrainSnapshot {
    /// Whether this exact runner lease was durably replaced by an addressed handoff.
    pub fn runner_lease_was_handed_off(&self, lease_id: RunnerLeaseId) -> bool;
}
pub enum BrainWireMessage { Snapshot, Event }
```

## Functions

```rust
pub fn directory_bytes(root: &Path) -> u64 { … }
pub fn observer_effect_audit_event(event: &BrainEvent) -> BrainEvent { … }
/// Per-Brain listing facts without hydrating or creating files.
pub fn summarize_unhydrated(root: Option<&Path>, name: &str, now_ms: u64) -> BrainListSummary { … }
```
