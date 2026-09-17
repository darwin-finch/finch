# ipc::codec — public interface

Generated from [`src/ipc/codec/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/ipc/codec/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub(crate) struct BrainRemoteCommand { … }
pub(crate) enum BrainRemoteCommandKind { Submit, Acknowledge, Detach, RequestRunnerHandoff, CancelRunnerHandoff, CancelRun, CreateSchedule, CancelSchedule, ScheduleInitialization }
pub(crate) enum BrainRemoteEnvelope { Projection, Command, Reply }
pub(crate) struct BrainRemoteMutation { … }
pub(crate) enum BrainRemoteReply { Submitted, Acknowledged, Detached, HandoffRequested, HandoffCancelled, RunCancelled, ScheduleCreated, ScheduleCancelled, InitializationScheduled, Error }
```

## Functions

```rust
pub(crate) fn brain_remote_command_fingerprint(kind: &BrainRemoteCommandKind) -> anyhow::Result<String> { … }
pub(crate) fn decode_approval_audience(reader: brain_approval_audience::Reader<'_>) -> anyhow::Result<BrainApprovalAudience> { … }
pub(crate) fn decode_attachment(reader: finch_ipc_capnp::brain_attachment::Reader<'_>) -> anyhow::Result<BrainAttachment> { … }
pub(crate) fn decode_brain_remote_envelope(bytes: &[u8]) -> anyhow::Result<BrainRemoteEnvelope> { … }
pub(crate) fn decode_brain_submission(reader: finch_ipc_capnp::brain_submission::Reader<'_>) -> anyhow::Result<BrainEventKind> { … }
pub(crate) fn decode_brain_wire_reader(root: finch_ipc_capnp::brain_wire_message::Reader<'_>) -> anyhow::Result<BrainWireMessage> { … }
pub(crate) fn decode_checkpoint(reader: wire::typed_runtime_checkpoint::Reader<'_>) -> Result<TypedRuntimeCheckpoint> { … }
/// Decode one durable typed-runtime checkpoint.
pub(crate) fn decode_checkpoint_bytes(encoded: &[u8]) -> Result<TypedRuntimeCheckpoint> { … }
pub(crate) fn decode_continuation_messages(messages: capnp::struct_list::Reader<finch_ipc_capnp::message::Owned>) -> anyhow::Result<Vec<crate::providers::Message>> { … }
pub(crate) fn decode_effect_record(reader: wire::brain_effect_record::Reader<'_>) -> Result<(uuid::Uuid, EffectJournalEntry)> { … }
pub(crate) fn decode_effects(reader: capnp::struct_list::Reader<'_, wire::capability_requirement::Owned>) -> Result<EffectSet> { … }
pub(crate) fn decode_environment(reader: finch_ipc_capnp::brain_environment::Reader<'_>) -> anyhow::Result<BrainEnvironment> { … }
pub(crate) fn decode_event(reader: finch_ipc_capnp::brain_event::Reader<'_>) -> anyhow::Result<BrainEvent> { … }
pub(crate) fn decode_invocation_metadata(reader: finch_ipc_capnp::invocation_metadata::Reader<'_>) -> anyhow::Result<crate::providers::InvocationMetadata> { … }
pub(crate) fn decode_json_value(reader: finch_ipc_capnp::json_value::Reader<'_>) -> anyhow::Result<serde_json::Value> { … }
pub(crate) fn decode_messages(messages: capnp::struct_list::Reader<finch_ipc_capnp::message::Owned>) -> anyhow::Result<Vec<crate::providers::Message>> { … }
/// Decode packed delivery frames.
pub(crate) fn decode_packed_delivery_envelopes(frames: capnp::data_list::Reader<'_>, journal: &[crate::server::RunnerEffectRecord]) -> Result<Vec<VmEffectEnvelope>> { … }
pub(crate) fn decode_packed_runtime_application_frames(frames: capnp::data_list::Reader<'_>) -> Result<Vec<RuntimeApplicationMessage>> { … }
pub(crate) fn decode_run(reader: finch_ipc_capnp::brain_run::Reader<'_>) -> anyhow::Result<BrainRun> { … }
pub(crate) fn decode_runner_handoff(reader: finch_ipc_capnp::brain_runner_handoff::Reader<'_>) -> anyhow::Result<BrainRunnerHandoff> { … }
pub(crate) fn decode_runner_lease(reader: finch_ipc_capnp::brain_runner_lease::Reader<'_>) -> anyhow::Result<BrainRunnerLease> { … }
pub(crate) fn decode_schedule(reader: finch_ipc_capnp::brain_schedule::Reader<'_>) -> anyhow::Result<BrainSchedule> { … }
pub(crate) fn decode_snapshot(reader: finch_ipc_capnp::brain_snapshot::Reader<'_>) -> anyhow::Result<BrainSnapshot> { … }
pub(crate) fn decode_value_list(reader: capnp::struct_list::Reader<'_, wire::typed_value::Owned>, depth: usize) -> Result<Vec<TypedValue>> { … }
pub(crate) fn decode_vm_side_effect(reader: wire::vm_side_effect::Reader<'_>) -> Result<VmSideEffect> { … }
pub(crate) fn encode_approval_audience(mut builder: brain_approval_audience::Builder<'_>, audience: &BrainApprovalAudience) { … }
pub(crate) fn encode_attachment(mut builder: finch_ipc_capnp::brain_attachment::Builder<'_>, attachment: &BrainAttachment) { … }
pub(crate) fn encode_brain_remote_envelope(envelope: &BrainRemoteEnvelope) -> anyhow::Result<Vec<u8>> { … }
pub(crate) fn encode_brain_submission(mut builder: finch_ipc_capnp::brain_submission::Builder<'_>, kind: &BrainEventKind) -> anyhow::Result<()> { … }
pub(crate) fn encode_brain_submission_outcome(mut builder: finch_ipc_capnp::brain_submission_outcome::Builder<'_>, accepted: &BrainEvent, run: Option<&BrainRun>, result: Option<&BrainEvent>) -> anyhow::Result<()> { … }
pub(crate) fn encode_checkpoint(mut builder: wire::typed_runtime_checkpoint::Builder<'_>, value: &TypedRuntimeCheckpoint) -> Result<()> { … }
/// Encode one durable typed-runtime checkpoint using the same closed native schema used by runner registration and result transport.
pub(crate) fn encode_checkpoint_bytes(value: &TypedRuntimeCheckpoint) -> Result<Vec<u8>> { … }
pub(crate) fn encode_continuation_messages(builder: capnp::struct_list::Builder<finch_ipc_capnp::message::Owned>, messages: &[crate::providers::Message]) -> anyhow::Result<()> { … }
pub(crate) fn encode_effect_record(mut builder: wire::brain_effect_record::Builder<'_>, execution_id: uuid::Uuid, entry: &EffectJournalEntry) -> Result<()> { … }
pub(crate) fn encode_effects(mut builder: capnp::struct_list::Builder<'_, wire::capability_requirement::Owned>, value: &EffectSet) { … }
pub(crate) fn encode_environment(mut builder: finch_ipc_capnp::brain_environment::Builder<'_>, environment: &BrainEnvironment) { … }
pub(crate) fn encode_event(mut builder: finch_ipc_capnp::brain_event::Builder<'_>, event: &BrainEvent) -> anyhow::Result<()> { … }
pub(crate) fn encode_invocation_metadata(mut builder: finch_ipc_capnp::invocation_metadata::Builder<'_>, metadata: &crate::providers::InvocationMetadata) { … }
pub(crate) fn encode_json_value(builder: finch_ipc_capnp::json_value::Builder<'_>, value: &serde_json::Value) -> anyhow::Result<()> { … }
pub(crate) fn encode_messages(mut builder: capnp::struct_list::Builder<finch_ipc_capnp::message::Owned>, messages: &[crate::providers::Message]) -> anyhow::Result<()> { … }
/// Packed envelopes derived from a runner effect journal.
pub(crate) fn encode_packed_delivery_envelopes(records: &[crate::server::RunnerEffectRecord]) -> Result<Vec<Vec<u8>>> { … }
pub(crate) fn encode_packed_runtime_application_frames(mut encoded: capnp::data_list::Builder<'_>, messages: &[RuntimeApplicationMessage]) -> Result<()> { … }
pub(crate) fn encode_run(mut builder: finch_ipc_capnp::brain_run::Builder<'_>, run: &BrainRun) { … }
pub(crate) fn encode_runner_handoff(mut builder: finch_ipc_capnp::brain_runner_handoff::Builder<'_>, handoff: &BrainRunnerHandoff) { … }
pub(crate) fn encode_runner_lease(mut builder: finch_ipc_capnp::brain_runner_lease::Builder<'_>, lease: &BrainRunnerLease) { … }
/// Compact packed Cap'n Proto frame for the Runtime/Application ABI.
pub(crate) fn encode_runtime_application_message_packed(value: &RuntimeApplicationMessage) -> Result<Vec<u8>> { … }
pub(crate) fn encode_schedule(mut builder: finch_ipc_capnp::brain_schedule::Builder<'_>, schedule: &BrainSchedule) { … }
pub(crate) fn encode_snapshot(mut builder: finch_ipc_capnp::brain_snapshot::Builder<'_>, snapshot: &BrainSnapshot) -> anyhow::Result<()> { … }
pub(crate) fn encode_value_list(mut builder: capnp::struct_list::Builder<'_, wire::typed_value::Owned>, values: &[TypedValue], depth: usize) -> Result<()> { … }
pub(crate) fn encode_vm_side_effect(mut builder: wire::vm_side_effect::Builder<'_>, value: &VmSideEffect) -> Result<()> { … }
pub(crate) fn run_status_from_capnp(status: finch_ipc_capnp::BrainRunStatus) -> BrainRunStatus { … }
pub(crate) fn run_status_to_capnp(status: BrainRunStatus) -> finch_ipc_capnp::BrainRunStatus { … }
```
