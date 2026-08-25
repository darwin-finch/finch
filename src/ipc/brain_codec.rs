use crate::brain::shared::{AttachmentId, AttachmentRole, BrainApprovalAudience, BrainId};
use crate::ipc::schema::finch_ipc_capnp::{self, brain_approval_audience};

fn attachment_role_to_capnp(role: AttachmentRole) -> finch_ipc_capnp::BrainAttachmentRole {
    match role {
        AttachmentRole::Runner => finch_ipc_capnp::BrainAttachmentRole::Runner,
        AttachmentRole::Driver => finch_ipc_capnp::BrainAttachmentRole::Driver,
        AttachmentRole::Consultant => finch_ipc_capnp::BrainAttachmentRole::Consultant,
        AttachmentRole::Observer => finch_ipc_capnp::BrainAttachmentRole::Observer,
    }
}

fn attachment_role_from_capnp(role: finch_ipc_capnp::BrainAttachmentRole) -> AttachmentRole {
    match role {
        finch_ipc_capnp::BrainAttachmentRole::Runner => AttachmentRole::Runner,
        finch_ipc_capnp::BrainAttachmentRole::Driver => AttachmentRole::Driver,
        finch_ipc_capnp::BrainAttachmentRole::Consultant => AttachmentRole::Consultant,
        finch_ipc_capnp::BrainAttachmentRole::Observer => AttachmentRole::Observer,
    }
}

pub(super) fn encode_approval_audience(
    mut builder: brain_approval_audience::Builder<'_>,
    audience: &BrainApprovalAudience,
) {
    builder.set_brain_id(&audience.brain_id.0.to_string());
    builder.set_brain(&audience.brain);
    builder.set_attachment_id(&audience.attachment_id.0.to_string());
    builder.set_subject(&audience.subject);
    builder.set_role(attachment_role_to_capnp(audience.role));
    builder.set_environment_generation(audience.environment_generation);
}

pub(super) fn decode_approval_audience(
    reader: brain_approval_audience::Reader<'_>,
) -> anyhow::Result<BrainApprovalAudience> {
    Ok(BrainApprovalAudience {
        brain_id: BrainId(uuid::Uuid::parse_str(reader.get_brain_id()?.to_str()?)?),
        brain: reader.get_brain()?.to_str()?.to_string(),
        attachment_id: AttachmentId(uuid::Uuid::parse_str(
            reader.get_attachment_id()?.to_str()?,
        )?),
        subject: reader.get_subject()?.to_str()?.to_string(),
        role: attachment_role_from_capnp(reader.get_role()?),
        environment_generation: reader.get_environment_generation(),
    })
}
