//! Cap'n Proto codecs for Brain remote envelopes and typed-runtime checkpoints.
//!
//! `brain_codec` and `checkpoint_codec` translate domain types into the
//! generated IPC schema and back. Framing and wire bytes belong here; RPC
//! dispatch stays in the parent client and server. The generated schema
//! (`crate::ipc::schema`) is IPC-owned, not codec-owned.

mod brain_codec;
mod checkpoint_codec;

pub(crate) use brain_codec::{
    brain_remote_command_fingerprint, decode_approval_audience, decode_attachment,
    decode_brain_remote_envelope, decode_brain_submission, decode_brain_wire_reader,
    decode_continuation_messages, decode_environment, decode_event, decode_invocation_metadata,
    decode_json_value, decode_messages, decode_run, decode_runner_handoff, decode_runner_lease,
    decode_schedule, decode_snapshot, encode_approval_audience, encode_attachment,
    encode_brain_remote_envelope, encode_brain_submission, encode_brain_submission_outcome,
    encode_continuation_messages, encode_environment, encode_event, encode_invocation_metadata,
    encode_json_value, encode_messages, encode_run, encode_runner_handoff, encode_runner_lease,
    encode_schedule, encode_snapshot, run_status_from_capnp, run_status_to_capnp,
    BrainRemoteCommand, BrainRemoteCommandKind, BrainRemoteEnvelope, BrainRemoteMutation,
    BrainRemoteReply,
};
pub(crate) use checkpoint_codec::{
    decode_checkpoint, decode_checkpoint_bytes, decode_effect_record, decode_effects,
    decode_packed_delivery_envelopes, decode_packed_runtime_application_frames, decode_value_list,
    decode_vm_side_effect, encode_checkpoint, encode_checkpoint_bytes, encode_effect_record,
    encode_effects, encode_packed_delivery_envelopes, encode_packed_runtime_application_frames,
    encode_runtime_application_message_packed, encode_value_list, encode_vm_side_effect,
};
