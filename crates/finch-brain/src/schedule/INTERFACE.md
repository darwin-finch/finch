# schedule — public interface

Generated from [`crates/finch-brain/src/schedule/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-brain/src/schedule/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Reviewed, immutable program that establishes a Brain's initial typed state.
pub struct BrainInitialization { … }
pub struct BrainSchedule { … }
pub enum BrainScheduleDeliveryPolicy { Coalesce, BoundedCatchUp }
/// One durable schedule delivery and the queued run that owns it.
pub struct BrainScheduleDue { … }
/// Durable, non-authority-bearing identity for a reviewed module scheduled by the Brain itself.
pub struct BrainScheduleModuleIdentity { … }
pub enum ProgramLanguage { Forth, Lisp }
pub struct ScheduleId(pub uuid::Uuid);
pub struct ScheduleIndex { … }
impl ScheduleIndex {
    /// Brains holding at least one schedule due at or before `now_ms`, in due order, each named once.
    pub fn due_brains(&self, now_ms: u64) -> Vec<String>;
    /// Diagnostic snapshot of every due key, never a rebuild.
    pub fn due_keys(&self) -> Vec<(u64, String, ScheduleId)>;
    /// Forget a Brain entirely, for removal and archival.
    pub fn forget(&mut self, name: &str);
    /// Whether this Brain currently has at least one active indexed schedule.
    pub fn has_active(&self, name: &str) -> bool;
    /// Whether this Brain's schedule set has been read into the index.
    pub fn is_indexed(&self, name: &str) -> bool;
    pub fn len(&self) -> usize;
    /// When the earliest active schedule in the store comes due.
    pub fn next_due_ms(&self) -> Option<u64>;
    /// Replace everything known about `name` with its current active schedules.
    pub fn reindex(&mut self, name: &str, schedules: &HashMap<ScheduleId, BrainSchedule>);
    /// Move or insert one schedule.
    pub fn upsert(&mut self, name: &str, schedule: &BrainSchedule);
}
```

## Functions

```rust
pub fn legacy_schedule_attachment_id() -> AttachmentId { … }
pub fn queued_schedule_run(schedule: &BrainSchedule, request_seq: u64, now_ms: u64) -> BrainRun { … }
pub fn schedule_due_window(schedule: &BrainSchedule, now_ms: u64) -> Result<(u32, u64, Option<u64>)> { … }
pub fn sorted_schedule_dues(dues: &HashMap<RunId, BrainScheduleDue>) -> Vec<BrainScheduleDue> { … }
pub fn sorted_schedules(schedules: &HashMap<ScheduleId, BrainSchedule>) -> Vec<BrainSchedule> { … }
```

## Constants

```rust
pub(crate) const DEFAULT_INITIALIZATION_SOURCE: &str = "(define (finch-brain-initialized) : int 1)";
pub const fn legacy_schedule_language() -> ProgramLanguage { … }
```
