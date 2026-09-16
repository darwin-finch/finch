# brain::attachment — public interface

Generated from [`src/brain/attachment/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/brain/attachment/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Stable identity of one client projection of a Brain.
pub struct AttachmentId(pub uuid::Uuid);
pub enum AttachmentRole { Runner, Driver, Consultant, Observer }
/// Exact participant/environment boundary to which a Brain-owned approval request is addressed.
pub struct BrainApprovalAudience { … }
pub struct BrainAttachment { … }
/// Identity of one live transport connection for a durable attachment.
pub struct ConnectionId(pub uuid::Uuid);
```

## Functions

```rust
/// Fold one attach/detach event into the live attachment map.
pub fn apply_event(attachments: &mut HashMap<AttachmentId, BrainAttachment>, event: &super::journal::BrainEvent) { … }
/// Load durable acknowledgement cursors.
pub fn read_cursors(root: Option<&Path>, name: &str, brain_id: BrainId) -> Result<HashMap<AttachmentId, u64>> { … }
pub fn sorted_attachments(attachments: &HashMap<AttachmentId, BrainAttachment>) -> Vec<BrainAttachment> { … }
/// Persist acknowledgement cursors for every known attachment.
pub fn write_cursors(root: Option<&Path>, name: &str, brain_id: BrainId, attachments: &HashMap<AttachmentId, BrainAttachment>) -> Result<()> { … }
```
