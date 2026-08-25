use crate::brain::store::{
    AttachmentId, AttachmentRole, BrainApprovalAudience, BrainAttachment, BrainEnvironment,
    BrainEvent, BrainEventKind, BrainId, BrainProgram, BrainRun, BrainRunKind, BrainRunStatus,
    BrainRunnerHandoff, BrainRunnerLease, BrainSchedule, BrainScheduleDeliveryPolicy,
    BrainScheduleDue, BrainSnapshot, BrainWireMessage, ConnectionId, ProgramLanguage, RunId,
    RunnerHandoffId, RunnerLeaseId, ScheduleId,
};
use crate::ipc::schema::finch_ipc_capnp::{self, brain_approval_audience};

const MAX_JSON_VALUE_DEPTH: usize = 64;

pub(super) fn encode_json_value(
    builder: finch_ipc_capnp::json_value::Builder<'_>,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    encode_json_value_at(builder, value, 0)
}

fn encode_json_value_at(
    mut builder: finch_ipc_capnp::json_value::Builder<'_>,
    value: &serde_json::Value,
    depth: usize,
) -> anyhow::Result<()> {
    if depth > MAX_JSON_VALUE_DEPTH {
        anyhow::bail!("dynamic value exceeds the maximum nesting depth");
    }
    match value {
        serde_json::Value::Null => builder.set_null_value(()),
        serde_json::Value::Bool(value) => builder.set_bool_value(*value),
        serde_json::Value::Number(value) if value.is_i64() => {
            builder.set_signed(value.as_i64().expect("checked signed JSON number"));
        }
        serde_json::Value::Number(value) if value.is_u64() => {
            builder.set_unsigned(value.as_u64().expect("checked unsigned JSON number"));
        }
        serde_json::Value::Number(value) => {
            builder.set_float(value.as_f64().expect("JSON number is representable as f64"));
        }
        serde_json::Value::String(value) => builder.set_text(value),
        serde_json::Value::Array(values) => {
            let mut encoded = builder.reborrow().init_array(values.len() as u32);
            for (index, value) in values.iter().enumerate() {
                encode_json_value_at(encoded.reborrow().get(index as u32), value, depth + 1)?;
            }
        }
        serde_json::Value::Object(values) => {
            let mut encoded = builder.reborrow().init_object(values.len() as u32);
            for (index, (name, value)) in values.iter().enumerate() {
                let mut field = encoded.reborrow().get(index as u32);
                field.set_name(name);
                encode_json_value_at(field.reborrow().init_value(), value, depth + 1)?;
            }
        }
    }
    Ok(())
}

pub(super) fn decode_json_value(
    reader: finch_ipc_capnp::json_value::Reader<'_>,
) -> anyhow::Result<serde_json::Value> {
    decode_json_value_at(reader, 0)
}

fn decode_json_value_at(
    reader: finch_ipc_capnp::json_value::Reader<'_>,
    depth: usize,
) -> anyhow::Result<serde_json::Value> {
    if depth > MAX_JSON_VALUE_DEPTH {
        anyhow::bail!("dynamic value exceeds the maximum nesting depth");
    }
    use finch_ipc_capnp::json_value::Which;
    Ok(match reader.which()? {
        Which::NullValue(()) => serde_json::Value::Null,
        Which::BoolValue(value) => serde_json::Value::Bool(value),
        Which::Signed(value) => serde_json::Value::Number(value.into()),
        Which::Unsigned(value) => serde_json::Value::Number(value.into()),
        Which::Float(value) => serde_json::Value::Number(
            serde_json::Number::from_f64(value)
                .ok_or_else(|| anyhow::anyhow!("dynamic value contains a non-finite float"))?,
        ),
        Which::Text(value) => serde_json::Value::String(text(value?)?),
        Which::Array(values) => serde_json::Value::Array(
            values?
                .iter()
                .map(|value| decode_json_value_at(value, depth + 1))
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        Which::Object(fields) => {
            let mut values = serde_json::Map::new();
            for field in fields?.iter() {
                values.insert(
                    text(field.get_name()?)?,
                    decode_json_value_at(field.get_value()?, depth + 1)?,
                );
            }
            serde_json::Value::Object(values)
        }
    })
}

pub(super) fn encode_messages(
    mut builder: capnp::struct_list::Builder<finch_ipc_capnp::message::Owned>,
    messages: &[crate::claude::Message],
) -> anyhow::Result<()> {
    for (message_index, message) in messages.iter().enumerate() {
        let mut encoded_message = builder.reborrow().get(message_index as u32);
        encoded_message.set_role(&message.role);
        let mut content = encoded_message.init_content(message.content.len() as u32);
        for (block_index, block) in message.content.iter().enumerate() {
            let mut encoded_block = content.reborrow().get(block_index as u32);
            match block {
                crate::claude::ContentBlock::Text { text } => encoded_block.set_text(text),
                crate::claude::ContentBlock::ToolUse { id, name, input } => {
                    let mut tool = encoded_block.init_tool_use();
                    tool.set_id(id);
                    tool.set_name(name);
                    encode_json_value(tool.reborrow().init_input(), input)?;
                }
                crate::claude::ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let mut result = encoded_block.init_tool_result();
                    result.set_tool_use_id(tool_use_id);
                    result.set_content(content);
                    result.set_is_error(is_error.unwrap_or(false));
                }
                crate::claude::ContentBlock::Image { .. } => {
                    // The current IPC schema has no image arm. Preserve the
                    // historical behavior rather than inventing a text form.
                    encoded_block.set_text("");
                }
            }
        }
    }
    Ok(())
}

pub(super) fn decode_messages(
    messages: capnp::struct_list::Reader<finch_ipc_capnp::message::Owned>,
) -> anyhow::Result<Vec<crate::claude::Message>> {
    let mut decoded = Vec::with_capacity(messages.len() as usize);
    for message in messages.iter() {
        let role = text(message.get_role()?)?;
        let mut content = Vec::new();
        for block in message.get_content()?.iter() {
            use finch_ipc_capnp::content_block::Which;
            match block.which()? {
                Which::Text(value) => content.push(crate::claude::ContentBlock::Text {
                    text: text(value?)?,
                }),
                Which::ToolUse(value) => {
                    let value = value?;
                    content.push(crate::claude::ContentBlock::ToolUse {
                        id: text(value.get_id()?)?,
                        name: text(value.get_name()?)?,
                        input: decode_json_value(value.get_input()?)?,
                    });
                }
                Which::ToolResult(value) => {
                    let value = value?;
                    content.push(crate::claude::ContentBlock::ToolResult {
                        tool_use_id: text(value.get_tool_use_id()?)?,
                        content: text(value.get_content()?)?,
                        is_error: Some(value.get_is_error()),
                    });
                }
                Which::Thinking(_) => {}
            }
        }
        decoded.push(crate::claude::Message { role, content });
    }
    Ok(decoded)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BrainRemoteCommand {
    pub request_id: u64,
    pub kind: BrainRemoteCommandKind,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BrainRemoteCommandKind {
    Submit(BrainEventKind),
    Acknowledge(u64),
    Detach,
    RequestRunnerHandoff {
        target_subject: String,
        expected_lease_id: RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
    },
    CancelRunnerHandoff(RunnerHandoffId),
    CancelRun(RunId),
    CreateSchedule {
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
    },
    CancelSchedule(ScheduleId),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BrainRemoteReply {
    Submitted {
        request_id: u64,
        accepted: BrainEvent,
        run: Option<BrainRun>,
        result: Option<BrainEvent>,
    },
    Acknowledged {
        request_id: u64,
        attachment: BrainAttachment,
    },
    Detached {
        request_id: u64,
    },
    HandoffRequested {
        request_id: u64,
        handoff: BrainRunnerHandoff,
    },
    HandoffCancelled {
        request_id: u64,
    },
    RunCancelled {
        request_id: u64,
        run: BrainRun,
    },
    ScheduleCreated {
        request_id: u64,
        schedule: BrainSchedule,
    },
    ScheduleCancelled {
        request_id: u64,
        cancelled: bool,
    },
    Error {
        request_id: u64,
        code: String,
        message: String,
    },
}

impl BrainRemoteReply {
    pub(crate) fn request_id(&self) -> u64 {
        match self {
            Self::Submitted { request_id, .. }
            | Self::Acknowledged { request_id, .. }
            | Self::Detached { request_id }
            | Self::HandoffRequested { request_id, .. }
            | Self::HandoffCancelled { request_id }
            | Self::RunCancelled { request_id, .. }
            | Self::ScheduleCreated { request_id, .. }
            | Self::ScheduleCancelled { request_id, .. }
            | Self::Error { request_id, .. } => *request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BrainRemoteEnvelope {
    Projection(BrainWireMessage),
    Command(BrainRemoteCommand),
    Reply(BrainRemoteReply),
}

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

fn language_to_capnp(language: ProgramLanguage) -> finch_ipc_capnp::ProgramLanguage {
    match language {
        ProgramLanguage::Forth => finch_ipc_capnp::ProgramLanguage::Forth,
        ProgramLanguage::Lisp => finch_ipc_capnp::ProgramLanguage::Lisp,
    }
}

fn language_from_capnp(language: finch_ipc_capnp::ProgramLanguage) -> ProgramLanguage {
    match language {
        finch_ipc_capnp::ProgramLanguage::Forth => ProgramLanguage::Forth,
        finch_ipc_capnp::ProgramLanguage::Lisp => ProgramLanguage::Lisp,
    }
}

fn run_kind_to_capnp(kind: BrainRunKind) -> finch_ipc_capnp::BrainRunKind {
    match kind {
        BrainRunKind::Interactive => finch_ipc_capnp::BrainRunKind::Interactive,
        BrainRunKind::Speculative => finch_ipc_capnp::BrainRunKind::Speculative,
        BrainRunKind::Scheduled => finch_ipc_capnp::BrainRunKind::Scheduled,
        BrainRunKind::Subagent => finch_ipc_capnp::BrainRunKind::Subagent,
        BrainRunKind::Maintenance => finch_ipc_capnp::BrainRunKind::Maintenance,
    }
}

fn run_kind_from_capnp(kind: finch_ipc_capnp::BrainRunKind) -> BrainRunKind {
    match kind {
        finch_ipc_capnp::BrainRunKind::Interactive => BrainRunKind::Interactive,
        finch_ipc_capnp::BrainRunKind::Speculative => BrainRunKind::Speculative,
        finch_ipc_capnp::BrainRunKind::Scheduled => BrainRunKind::Scheduled,
        finch_ipc_capnp::BrainRunKind::Subagent => BrainRunKind::Subagent,
        finch_ipc_capnp::BrainRunKind::Maintenance => BrainRunKind::Maintenance,
    }
}

fn run_status_to_capnp(status: BrainRunStatus) -> finch_ipc_capnp::BrainRunStatus {
    match status {
        BrainRunStatus::QueuedForEnvironment => {
            finch_ipc_capnp::BrainRunStatus::QueuedForEnvironment
        }
        BrainRunStatus::Running => finch_ipc_capnp::BrainRunStatus::Running,
        BrainRunStatus::AwaitingApproval => finch_ipc_capnp::BrainRunStatus::AwaitingApproval,
        BrainRunStatus::Completed => finch_ipc_capnp::BrainRunStatus::Completed,
        BrainRunStatus::Failed => finch_ipc_capnp::BrainRunStatus::Failed,
        BrainRunStatus::Cancelled => finch_ipc_capnp::BrainRunStatus::Cancelled,
        BrainRunStatus::Interrupted => finch_ipc_capnp::BrainRunStatus::Interrupted,
    }
}

fn run_status_from_capnp(status: finch_ipc_capnp::BrainRunStatus) -> BrainRunStatus {
    match status {
        finch_ipc_capnp::BrainRunStatus::QueuedForEnvironment => {
            BrainRunStatus::QueuedForEnvironment
        }
        finch_ipc_capnp::BrainRunStatus::Running => BrainRunStatus::Running,
        finch_ipc_capnp::BrainRunStatus::AwaitingApproval => BrainRunStatus::AwaitingApproval,
        finch_ipc_capnp::BrainRunStatus::Completed => BrainRunStatus::Completed,
        finch_ipc_capnp::BrainRunStatus::Failed => BrainRunStatus::Failed,
        finch_ipc_capnp::BrainRunStatus::Cancelled => BrainRunStatus::Cancelled,
        finch_ipc_capnp::BrainRunStatus::Interrupted => BrainRunStatus::Interrupted,
    }
}

fn schedule_policy_kind_to_capnp(
    policy: &BrainScheduleDeliveryPolicy,
) -> finch_ipc_capnp::BrainSchedulePolicyKind {
    match policy {
        BrainScheduleDeliveryPolicy::Coalesce => {
            finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce
        }
        BrainScheduleDeliveryPolicy::BoundedCatchUp { .. } => {
            finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp
        }
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

pub(super) fn encode_brain_submission(
    mut builder: finch_ipc_capnp::brain_submission::Builder<'_>,
    kind: &BrainEventKind,
) -> anyhow::Result<()> {
    match kind {
        BrainEventKind::Prompt { text } => builder.set_prompt(text),
        BrainEventKind::ParticipantMessage { text } => builder.set_participant_message(text),
        BrainEventKind::Program { language, source } => {
            let mut program = builder.init_program();
            program.set_language(language_to_capnp(*language));
            program.set_source(source);
        }
        BrainEventKind::ProgramPopped { program_seq } => {
            builder.set_program_popped(*program_seq);
        }
        BrainEventKind::ApprovalDecided {
            request_seq,
            approval_id,
            decision,
        } => {
            let mut decided = builder.init_approval_decided();
            decided.set_request_seq(*request_seq);
            decided.set_approval_id(approval_id);
            encode_json_value(decided.reborrow().init_decision(), decision)?;
        }
        _ => anyhow::bail!("internal Brain events cannot be encoded as participant submissions"),
    }
    Ok(())
}

pub(super) fn decode_brain_submission(
    reader: finch_ipc_capnp::brain_submission::Reader<'_>,
) -> anyhow::Result<BrainEventKind> {
    use finch_ipc_capnp::brain_submission::Which;

    Ok(match reader.which()? {
        Which::Prompt(value) => BrainEventKind::Prompt { text: text(value?)? },
        Which::ParticipantMessage(value) => BrainEventKind::ParticipantMessage {
            text: text(value?)?,
        },
        Which::Program(program) => {
            let program = program?;
            BrainEventKind::Program {
                language: language_from_capnp(program.get_language()?),
                source: text(program.get_source()?)?,
            }
        }
        Which::ProgramPopped(program_seq) => BrainEventKind::ProgramPopped { program_seq },
        Which::ApprovalDecided(decided) => {
            let decided = decided?;
            BrainEventKind::ApprovalDecided {
                request_seq: decided.get_request_seq(),
                approval_id: text(decided.get_approval_id()?)?,
                decision: decode_json_value(decided.get_decision()?)?,
            }
        }
    })
}

pub(super) fn encode_brain_submission_outcome(
    mut builder: finch_ipc_capnp::brain_submission_outcome::Builder<'_>,
    accepted: &BrainEvent,
    run: Option<&BrainRun>,
    result: Option<&BrainEvent>,
) -> anyhow::Result<()> {
    encode_event(builder.reborrow().init_accepted(), accepted)?;
    if let Some(run) = run {
        builder.set_has_run(true);
        encode_run(builder.reborrow().init_run(), run);
    }
    if let Some(result) = result {
        builder.set_has_result(true);
        encode_event(builder.reborrow().init_result(), result)?;
    }
    Ok(())
}

fn decode_brain_submission_outcome(
    reader: finch_ipc_capnp::brain_submission_outcome::Reader<'_>,
) -> anyhow::Result<(BrainEvent, Option<BrainRun>, Option<BrainEvent>)> {
    let accepted = decode_event(reader.get_accepted()?)?;
    let run = reader
        .get_has_run()
        .then(|| decode_run(reader.get_run()?))
        .transpose()?;
    let result = reader
        .get_has_result()
        .then(|| decode_event(reader.get_result()?))
        .transpose()?;
    Ok((accepted, run, result))
}

#[cfg(test)]
pub(crate) fn encode_brain_wire_message(message: &BrainWireMessage) -> anyhow::Result<Vec<u8>> {
    let mut encoded = capnp::message::Builder::new_default();
    let root = encoded.init_root::<finch_ipc_capnp::brain_wire_message::Builder<'_>>();
    match message {
        BrainWireMessage::Snapshot { brain } => encode_snapshot(root.init_snapshot(), brain)?,
        BrainWireMessage::Event { event } => encode_event(root.init_event(), event)?,
    }
    Ok(capnp::serialize::write_message_to_words(&encoded))
}

#[cfg(test)]
pub(crate) fn decode_brain_wire_message(bytes: &[u8]) -> anyhow::Result<BrainWireMessage> {
    let mut cursor = std::io::Cursor::new(bytes);
    let encoded =
        capnp::serialize::read_message(&mut cursor, capnp::message::ReaderOptions::new())?;
    let root = encoded.get_root::<finch_ipc_capnp::brain_wire_message::Reader<'_>>()?;
    decode_brain_wire_reader(root)
}

pub(super) fn decode_brain_wire_reader(
    root: finch_ipc_capnp::brain_wire_message::Reader<'_>,
) -> anyhow::Result<BrainWireMessage> {
    match root.which()? {
        finch_ipc_capnp::brain_wire_message::Snapshot(snapshot) => Ok(BrainWireMessage::Snapshot {
            brain: decode_snapshot(snapshot?)?,
        }),
        finch_ipc_capnp::brain_wire_message::Event(event) => Ok(BrainWireMessage::Event {
            event: decode_event(event?)?,
        }),
    }
}

pub(crate) fn encode_brain_remote_envelope(
    envelope: &BrainRemoteEnvelope,
) -> anyhow::Result<Vec<u8>> {
    let mut encoded = capnp::message::Builder::new_default();
    let mut root = encoded.init_root::<finch_ipc_capnp::brain_remote_envelope::Builder<'_>>();
    match envelope {
        BrainRemoteEnvelope::Projection(message) => {
            let projection = root.reborrow().init_projection();
            match message {
                BrainWireMessage::Snapshot { brain } => {
                    encode_snapshot(projection.init_snapshot(), brain)?
                }
                BrainWireMessage::Event { event } => encode_event(projection.init_event(), event)?,
            }
        }
        BrainRemoteEnvelope::Command(command) => {
            let mut builder = root.reborrow().init_command();
            builder.set_request_id(command.request_id);
            match &command.kind {
                BrainRemoteCommandKind::Submit(kind) => {
                    encode_brain_submission(builder.init_submit(), kind)?
                }
                BrainRemoteCommandKind::Acknowledge(seq) => builder.set_acknowledge(*seq),
                BrainRemoteCommandKind::Detach => builder.set_detach(()),
                BrainRemoteCommandKind::RequestRunnerHandoff {
                    target_subject,
                    expected_lease_id,
                    environment_generation,
                    ttl_ms,
                } => {
                    let mut request = builder.init_request_runner_handoff();
                    request.set_target_subject(target_subject);
                    request.set_expected_lease_id(&expected_lease_id.0.to_string());
                    request.set_environment_generation(*environment_generation);
                    request.set_ttl_ms(*ttl_ms);
                }
                BrainRemoteCommandKind::CancelRunnerHandoff(handoff_id) => {
                    builder.set_cancel_runner_handoff(&handoff_id.0.to_string())
                }
                BrainRemoteCommandKind::CancelRun(run_id) => {
                    builder.set_cancel_run(&run_id.0.to_string())
                }
                BrainRemoteCommandKind::CreateSchedule {
                    language,
                    source,
                    grant_ceiling,
                    next_due_ms,
                    interval_ms,
                    delivery_policy,
                } => {
                    let mut request = builder.init_create_schedule();
                    request.set_language(language_to_capnp(*language));
                    request.set_source(source);
                    crate::ipc::checkpoint_codec::encode_effects(
                        request
                            .reborrow()
                            .init_grant_ceiling(grant_ceiling.0.len() as u32),
                        grant_ceiling,
                    );
                    request.set_next_due_ms(*next_due_ms);
                    if let Some(interval_ms) = interval_ms {
                        request.set_has_interval_ms(true);
                        request.set_interval_ms(*interval_ms);
                    }
                    let mut policy = request.reborrow().init_policy();
                    policy.set_kind(schedule_policy_kind_to_capnp(delivery_policy));
                    if let BrainScheduleDeliveryPolicy::BoundedCatchUp {
                        max_catch_up,
                        expires_after_ms,
                    } = delivery_policy
                    {
                        policy.set_max_catch_up(*max_catch_up);
                        policy.set_expires_after_ms(*expires_after_ms);
                    }
                }
                BrainRemoteCommandKind::CancelSchedule(schedule_id) => {
                    builder.set_cancel_schedule(&schedule_id.0.to_string())
                }
            }
        }
        BrainRemoteEnvelope::Reply(reply) => {
            let mut builder = root.reborrow().init_reply();
            builder.set_request_id(reply.request_id());
            match reply {
                BrainRemoteReply::Submitted {
                    accepted,
                    run,
                    result,
                    ..
                } => encode_brain_submission_outcome(
                    builder.init_submitted(),
                    accepted,
                    run.as_ref(),
                    result.as_ref(),
                )?,
                BrainRemoteReply::Acknowledged { attachment, .. } => {
                    encode_attachment(builder.init_acknowledged(), attachment)
                }
                BrainRemoteReply::Detached { .. } => builder.set_detached(()),
                BrainRemoteReply::HandoffRequested { handoff, .. } => {
                    encode_runner_handoff(builder.init_handoff_requested(), handoff)
                }
                BrainRemoteReply::HandoffCancelled { .. } => {
                    builder.set_handoff_cancelled(())
                }
                BrainRemoteReply::RunCancelled { run, .. } => {
                    encode_run(builder.init_run_cancelled(), run)
                }
                BrainRemoteReply::ScheduleCreated { schedule, .. } => {
                    encode_schedule(builder.init_schedule_created(), schedule)
                }
                BrainRemoteReply::ScheduleCancelled { cancelled, .. } => {
                    builder.set_schedule_cancelled(*cancelled)
                }
                BrainRemoteReply::Error { code, message, .. } => {
                    let mut error = builder.init_error();
                    error.set_code(code);
                    error.set_message(message);
                }
            }
        }
    }
    Ok(capnp::serialize::write_message_to_words(&encoded))
}

pub(crate) fn decode_brain_remote_envelope(bytes: &[u8]) -> anyhow::Result<BrainRemoteEnvelope> {
    use finch_ipc_capnp::brain_remote_command::Which as CommandWhich;
    use finch_ipc_capnp::brain_remote_envelope::Which as EnvelopeWhich;
    use finch_ipc_capnp::brain_remote_reply::Which as ReplyWhich;

    let mut cursor = std::io::Cursor::new(bytes);
    let encoded =
        capnp::serialize::read_message(&mut cursor, capnp::message::ReaderOptions::new())?;
    let root = encoded.get_root::<finch_ipc_capnp::brain_remote_envelope::Reader<'_>>()?;
    Ok(match root.which()? {
        EnvelopeWhich::Projection(projection) => {
            BrainRemoteEnvelope::Projection(decode_brain_wire_reader(projection?)?)
        }
        EnvelopeWhich::Command(command) => {
            let command = command?;
            let request_id = command.get_request_id();
            let kind = match command.which()? {
                CommandWhich::Submit(submission) => {
                    BrainRemoteCommandKind::Submit(decode_brain_submission(submission?)?)
                }
                CommandWhich::Acknowledge(seq) => BrainRemoteCommandKind::Acknowledge(seq),
                CommandWhich::Detach(()) => BrainRemoteCommandKind::Detach,
                CommandWhich::RequestRunnerHandoff(request) => {
                    let request = request?;
                    BrainRemoteCommandKind::RequestRunnerHandoff {
                        target_subject: text(request.get_target_subject()?)?,
                        expected_lease_id: RunnerLeaseId(parse_uuid(
                            request.get_expected_lease_id()?,
                        )?),
                        environment_generation: request.get_environment_generation(),
                        ttl_ms: request.get_ttl_ms(),
                    }
                }
                CommandWhich::CancelRunnerHandoff(handoff_id) => {
                    BrainRemoteCommandKind::CancelRunnerHandoff(RunnerHandoffId(parse_uuid(
                        handoff_id?,
                    )?))
                }
                CommandWhich::CancelRun(run_id) => {
                    BrainRemoteCommandKind::CancelRun(RunId(parse_uuid(run_id?)?))
                }
                CommandWhich::CreateSchedule(request) => {
                    let request = request?;
                    let policy = request.get_policy()?;
                    let delivery_policy = match policy.get_kind()? {
                        finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce => {
                            BrainScheduleDeliveryPolicy::Coalesce
                        }
                        finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp => {
                            BrainScheduleDeliveryPolicy::BoundedCatchUp {
                                max_catch_up: policy.get_max_catch_up(),
                                expires_after_ms: policy.get_expires_after_ms(),
                            }
                        }
                    };
                    BrainRemoteCommandKind::CreateSchedule {
                        language: language_from_capnp(request.get_language()?),
                        source: text(request.get_source()?)?,
                        grant_ceiling: crate::ipc::checkpoint_codec::decode_effects(
                            request.get_grant_ceiling()?,
                        )?,
                        next_due_ms: request.get_next_due_ms(),
                        interval_ms: request
                            .get_has_interval_ms()
                            .then(|| request.get_interval_ms()),
                        delivery_policy,
                    }
                }
                CommandWhich::CancelSchedule(schedule_id) => {
                    BrainRemoteCommandKind::CancelSchedule(ScheduleId(parse_uuid(schedule_id?)?))
                }
            };
            BrainRemoteEnvelope::Command(BrainRemoteCommand { request_id, kind })
        }
        EnvelopeWhich::Reply(reply) => {
            let reply = reply?;
            let request_id = reply.get_request_id();
            BrainRemoteEnvelope::Reply(match reply.which()? {
                ReplyWhich::Submitted(outcome) => {
                    let (accepted, run, result) = decode_brain_submission_outcome(outcome?)?;
                    BrainRemoteReply::Submitted {
                        request_id,
                        accepted,
                        run,
                        result,
                    }
                }
                ReplyWhich::Acknowledged(attachment) => BrainRemoteReply::Acknowledged {
                    request_id,
                    attachment: decode_attachment(attachment?)?,
                },
                ReplyWhich::Detached(()) => BrainRemoteReply::Detached { request_id },
                ReplyWhich::HandoffRequested(handoff) => BrainRemoteReply::HandoffRequested {
                    request_id,
                    handoff: decode_runner_handoff(handoff?)?,
                },
                ReplyWhich::HandoffCancelled(()) => {
                    BrainRemoteReply::HandoffCancelled { request_id }
                }
                ReplyWhich::RunCancelled(run) => BrainRemoteReply::RunCancelled {
                    request_id,
                    run: decode_run(run?)?,
                },
                ReplyWhich::ScheduleCreated(schedule) => BrainRemoteReply::ScheduleCreated {
                    request_id,
                    schedule: decode_schedule(schedule?)?,
                },
                ReplyWhich::ScheduleCancelled(cancelled) => {
                    BrainRemoteReply::ScheduleCancelled {
                        request_id,
                        cancelled,
                    }
                }
                ReplyWhich::Error(error) => {
                    let error = error?;
                    BrainRemoteReply::Error {
                        request_id,
                        code: text(error.get_code()?)?,
                        message: text(error.get_message()?)?,
                    }
                }
            })
        }
    })
}

pub(super) fn encode_snapshot(
    mut builder: finch_ipc_capnp::brain_snapshot::Builder<'_>,
    snapshot: &BrainSnapshot,
) -> anyhow::Result<()> {
    builder.set_brain_id(&snapshot.brain_id.0.to_string());
    builder.set_name(&snapshot.name);
    encode_environment(builder.reborrow().init_environment(), &snapshot.environment);
    builder.set_revision(snapshot.revision);
    let mut events = builder.reborrow().init_events(snapshot.events.len() as u32);
    for (index, event) in snapshot.events.iter().enumerate() {
        encode_event(events.reborrow().get(index as u32), event)?;
    }
    let mut programs = builder
        .reborrow()
        .init_program_stack(snapshot.program_stack.len() as u32);
    for (index, program) in snapshot.program_stack.iter().enumerate() {
        encode_program(programs.reborrow().get(index as u32), program);
    }
    let mut attachments = builder
        .reborrow()
        .init_attachments(snapshot.attachments.len() as u32);
    for (index, attachment) in snapshot.attachments.iter().enumerate() {
        encode_attachment(attachments.reborrow().get(index as u32), attachment);
    }
    if let Some(lease) = &snapshot.runner_lease {
        builder.set_has_runner_lease(true);
        encode_runner_lease(builder.reborrow().init_runner_lease(), lease);
    }
    if let Some(handoff) = &snapshot.runner_handoff {
        builder.set_has_runner_handoff(true);
        encode_runner_handoff(builder.reborrow().init_runner_handoff(), handoff);
    }
    let mut runs = builder.reborrow().init_runs(snapshot.runs.len() as u32);
    for (index, run) in snapshot.runs.iter().enumerate() {
        encode_run(runs.reborrow().get(index as u32), run);
    }
    let mut schedules = builder
        .reborrow()
        .init_schedules(snapshot.schedules.len() as u32);
    for (index, schedule) in snapshot.schedules.iter().enumerate() {
        encode_schedule(schedules.reborrow().get(index as u32), schedule);
    }
    let mut dues = builder
        .reborrow()
        .init_pending_schedule_dues(snapshot.pending_schedule_dues.len() as u32);
    for (index, due) in snapshot.pending_schedule_dues.iter().enumerate() {
        encode_schedule_due(dues.reborrow().get(index as u32), due);
    }
    Ok(())
}

pub(super) fn decode_snapshot(
    reader: finch_ipc_capnp::brain_snapshot::Reader<'_>,
) -> anyhow::Result<BrainSnapshot> {
    let events = reader
        .get_events()?
        .iter()
        .map(decode_event)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let program_stack = reader
        .get_program_stack()?
        .iter()
        .map(decode_program)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let attachments = reader
        .get_attachments()?
        .iter()
        .map(decode_attachment)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let runs = reader
        .get_runs()?
        .iter()
        .map(decode_run)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let schedules = reader
        .get_schedules()?
        .iter()
        .map(decode_schedule)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let pending_schedule_dues = reader
        .get_pending_schedule_dues()?
        .iter()
        .map(decode_schedule_due)
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(BrainSnapshot {
        brain_id: parse_brain_id(reader.get_brain_id()?)?,
        name: text(reader.get_name()?)?,
        environment: decode_environment(reader.get_environment()?)?,
        revision: reader.get_revision(),
        events,
        program_stack,
        attachments,
        runner_lease: reader
            .get_has_runner_lease()
            .then(|| reader.get_runner_lease())
            .transpose()?
            .map(decode_runner_lease)
            .transpose()?,
        runner_handoff: reader
            .get_has_runner_handoff()
            .then(|| reader.get_runner_handoff())
            .transpose()?
            .map(decode_runner_handoff)
            .transpose()?,
        runs,
        schedules,
        pending_schedule_dues,
    })
}

pub(super) fn encode_environment(
    mut builder: finch_ipc_capnp::brain_environment::Builder<'_>,
    environment: &BrainEnvironment,
) {
    builder.set_machine(&environment.machine);
    builder.set_workspace(&environment.workspace.to_string_lossy());
    builder.set_generation(environment.generation);
}

pub(super) fn decode_environment(
    reader: finch_ipc_capnp::brain_environment::Reader<'_>,
) -> anyhow::Result<BrainEnvironment> {
    Ok(BrainEnvironment {
        machine: text(reader.get_machine()?)?,
        workspace: text(reader.get_workspace()?)?.into(),
        generation: reader.get_generation(),
    })
}

pub(super) fn encode_attachment(
    mut builder: finch_ipc_capnp::brain_attachment::Builder<'_>,
    attachment: &BrainAttachment,
) {
    builder.set_attachment_id(&attachment.attachment_id.0.to_string());
    builder.set_subject(&attachment.subject);
    builder.set_role(attachment_role_to_capnp(attachment.role));
    builder.set_acknowledged_seq(attachment.acknowledged_seq);
    builder.set_connected(attachment.connected);
    if let Some(connection_id) = attachment.connection_id {
        builder.set_has_connection(true);
        builder.set_connection_id(&connection_id.0.to_string());
    }
}

pub(super) fn decode_attachment(
    reader: finch_ipc_capnp::brain_attachment::Reader<'_>,
) -> anyhow::Result<BrainAttachment> {
    Ok(BrainAttachment {
        attachment_id: AttachmentId(parse_uuid(reader.get_attachment_id()?)?),
        subject: text(reader.get_subject()?)?,
        role: attachment_role_from_capnp(reader.get_role()?),
        acknowledged_seq: reader.get_acknowledged_seq(),
        connected: reader.get_connected(),
        connection_id: reader
            .get_has_connection()
            .then(|| reader.get_connection_id())
            .transpose()?
            .map(parse_uuid)
            .transpose()?
            .map(ConnectionId),
    })
}

pub(super) fn encode_runner_lease(
    mut builder: finch_ipc_capnp::brain_runner_lease::Builder<'_>,
    lease: &BrainRunnerLease,
) {
    builder.set_lease_id(&lease.lease_id.0.to_string());
    builder.set_subject(&lease.subject);
    builder.set_environment_generation(lease.environment_generation);
    builder.set_acquired_ms(lease.acquired_ms);
    builder.set_expires_ms(lease.expires_ms);
}

pub(super) fn decode_runner_lease(
    reader: finch_ipc_capnp::brain_runner_lease::Reader<'_>,
) -> anyhow::Result<BrainRunnerLease> {
    Ok(BrainRunnerLease {
        lease_id: RunnerLeaseId(parse_uuid(reader.get_lease_id()?)?),
        subject: text(reader.get_subject()?)?,
        environment_generation: reader.get_environment_generation(),
        acquired_ms: reader.get_acquired_ms(),
        expires_ms: reader.get_expires_ms(),
    })
}

pub(super) fn encode_runner_handoff(
    mut builder: finch_ipc_capnp::brain_runner_handoff::Builder<'_>,
    handoff: &BrainRunnerHandoff,
) {
    builder.set_handoff_id(&handoff.handoff_id.0.to_string());
    builder.set_from_lease_id(&handoff.from_lease_id.0.to_string());
    builder.set_requested_by(&handoff.requested_by);
    builder.set_target_subject(&handoff.target_subject);
    builder.set_environment_generation(handoff.environment_generation);
    builder.set_requested_ms(handoff.requested_ms);
    builder.set_expires_ms(handoff.expires_ms);
}

pub(super) fn decode_runner_handoff(
    reader: finch_ipc_capnp::brain_runner_handoff::Reader<'_>,
) -> anyhow::Result<BrainRunnerHandoff> {
    Ok(BrainRunnerHandoff {
        handoff_id: RunnerHandoffId(parse_uuid(reader.get_handoff_id()?)?),
        from_lease_id: RunnerLeaseId(parse_uuid(reader.get_from_lease_id()?)?),
        requested_by: text(reader.get_requested_by()?)?,
        target_subject: text(reader.get_target_subject()?)?,
        environment_generation: reader.get_environment_generation(),
        requested_ms: reader.get_requested_ms(),
        expires_ms: reader.get_expires_ms(),
    })
}

fn encode_program(
    mut builder: finch_ipc_capnp::brain_program::Builder<'_>,
    program: &BrainProgram,
) {
    builder.set_seq(program.seq);
    builder.set_sender(&program.sender);
    builder.set_language(language_to_capnp(program.language));
    builder.set_source(&program.source);
}

fn decode_program(
    reader: finch_ipc_capnp::brain_program::Reader<'_>,
) -> anyhow::Result<BrainProgram> {
    Ok(BrainProgram {
        seq: reader.get_seq(),
        sender: text(reader.get_sender()?)?,
        language: language_from_capnp(reader.get_language()?),
        source: text(reader.get_source()?)?,
    })
}

pub(super) fn encode_run(mut builder: finch_ipc_capnp::brain_run::Builder<'_>, run: &BrainRun) {
    builder.set_run_id(&run.run_id.0.to_string());
    builder.set_kind(run_kind_to_capnp(run.kind));
    if let Some(parent_run_id) = run.parent_run_id {
        builder.set_has_parent_run_id(true);
        builder.set_parent_run_id(&parent_run_id.0.to_string());
    }
    builder.set_request_seq(run.request_seq);
    builder.set_initiating_attachment_id(&run.initiating_attachment_id.0.to_string());
    builder.set_initiated_by(&run.initiated_by);
    builder.set_status(run_status_to_capnp(run.status));
    builder.set_started_ms(run.started_ms);
    builder.set_updated_ms(run.updated_ms);
    if let Some(detail) = &run.detail {
        builder.set_has_detail(true);
        builder.set_detail(detail);
    }
}

pub(super) fn decode_run(
    reader: finch_ipc_capnp::brain_run::Reader<'_>,
) -> anyhow::Result<BrainRun> {
    Ok(BrainRun {
        run_id: RunId(parse_uuid(reader.get_run_id()?)?),
        kind: run_kind_from_capnp(reader.get_kind()?),
        parent_run_id: reader
            .get_has_parent_run_id()
            .then(|| reader.get_parent_run_id())
            .transpose()?
            .map(parse_uuid)
            .transpose()?
            .map(RunId),
        request_seq: reader.get_request_seq(),
        initiating_attachment_id: AttachmentId(parse_uuid(reader.get_initiating_attachment_id()?)?),
        initiated_by: text(reader.get_initiated_by()?)?,
        status: run_status_from_capnp(reader.get_status()?),
        started_ms: reader.get_started_ms(),
        updated_ms: reader.get_updated_ms(),
        detail: reader
            .get_has_detail()
            .then(|| reader.get_detail())
            .transpose()?
            .map(text)
            .transpose()?,
    })
}

pub(crate) fn encode_schedule(
    mut builder: finch_ipc_capnp::brain_schedule::Builder<'_>,
    schedule: &BrainSchedule,
) {
    builder.set_schedule_id(&schedule.schedule_id.0.to_string());
    builder.set_initiating_attachment_id(&schedule.initiating_attachment_id.0.to_string());
    builder.set_created_by(&schedule.created_by);
    crate::ipc::checkpoint_codec::encode_effects(
        builder
            .reborrow()
            .init_grant_ceiling(schedule.grant_ceiling.0.len() as u32),
        &schedule.grant_ceiling,
    );
    builder.set_language(language_to_capnp(schedule.language));
    builder.set_source(&schedule.source);
    builder.set_next_due_ms(schedule.next_due_ms);
    if let Some(interval_ms) = schedule.interval_ms {
        builder.set_has_interval_ms(true);
        builder.set_interval_ms(interval_ms);
    }
    let mut policy = builder.reborrow().init_delivery_policy();
    policy.set_kind(schedule_policy_kind_to_capnp(&schedule.delivery_policy));
    if let BrainScheduleDeliveryPolicy::BoundedCatchUp {
        max_catch_up,
        expires_after_ms,
    } = &schedule.delivery_policy
    {
        policy.set_max_catch_up(*max_catch_up);
        policy.set_expires_after_ms(*expires_after_ms);
    }
    builder.set_active(schedule.active);
}

pub(crate) fn decode_schedule(
    reader: finch_ipc_capnp::brain_schedule::Reader<'_>,
) -> anyhow::Result<BrainSchedule> {
    let policy = reader.get_delivery_policy()?;
    let delivery_policy = match policy.get_kind()? {
        finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce => {
            BrainScheduleDeliveryPolicy::Coalesce
        }
        finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp => {
            BrainScheduleDeliveryPolicy::BoundedCatchUp {
                max_catch_up: policy.get_max_catch_up(),
                expires_after_ms: policy.get_expires_after_ms(),
            }
        }
    };
    Ok(BrainSchedule {
        schedule_id: ScheduleId(parse_uuid(reader.get_schedule_id()?)?),
        initiating_attachment_id: AttachmentId(parse_uuid(
            reader.get_initiating_attachment_id()?,
        )?),
        created_by: text(reader.get_created_by()?)?,
        grant_ceiling: crate::ipc::checkpoint_codec::decode_effects(
            reader.get_grant_ceiling()?,
        )?,
        language: language_from_capnp(reader.get_language()?),
        source: text(reader.get_source()?)?,
        next_due_ms: reader.get_next_due_ms(),
        interval_ms: reader
            .get_has_interval_ms()
            .then(|| reader.get_interval_ms()),
        delivery_policy,
        active: reader.get_active(),
    })
}

fn encode_schedule_due(
    mut builder: finch_ipc_capnp::brain_schedule_due::Builder<'_>,
    due: &BrainScheduleDue,
) {
    builder.set_schedule_id(&due.schedule_id.0.to_string());
    encode_run(builder.reborrow().init_run(), &due.run);
    builder.set_language(language_to_capnp(due.language));
    builder.set_source(&due.source);
    crate::ipc::checkpoint_codec::encode_effects(
        builder
            .reborrow()
            .init_grant_ceiling(due.grant_ceiling.0.len() as u32),
        &due.grant_ceiling,
    );
    builder.set_due_at_ms(due.due_at_ms);
    builder.set_first_missed_at_ms(due.first_missed_at_ms);
    builder.set_missed_count(due.missed_count);
    builder.set_has_next_due_ms(due.next_due_ms.is_some());
    builder.set_next_due_ms(due.next_due_ms.unwrap_or_default());
}

fn decode_schedule_due(
    reader: finch_ipc_capnp::brain_schedule_due::Reader<'_>,
) -> anyhow::Result<BrainScheduleDue> {
    Ok(BrainScheduleDue {
        schedule_id: ScheduleId(parse_uuid(reader.get_schedule_id()?)?),
        run: decode_run(reader.get_run()?)?,
        language: language_from_capnp(reader.get_language()?),
        source: text(reader.get_source()?)?,
        grant_ceiling: crate::ipc::checkpoint_codec::decode_effects(
            reader.get_grant_ceiling()?,
        )?,
        due_at_ms: reader.get_due_at_ms(),
        first_missed_at_ms: reader.get_first_missed_at_ms(),
        missed_count: reader.get_missed_count(),
        next_due_ms: reader
            .get_has_next_due_ms()
            .then(|| reader.get_next_due_ms()),
    })
}

pub(super) fn encode_event(
    mut builder: finch_ipc_capnp::brain_event::Builder<'_>,
    event: &BrainEvent,
) -> anyhow::Result<()> {
    builder.set_schema_version(event.schema_version);
    builder.set_brain_id(&event.brain_id.0.to_string());
    builder.set_seq(event.seq);
    builder.set_environment_generation(event.environment_generation);
    builder.set_sender(&event.sender);
    builder.set_created_ms(event.created_ms);
    match &event.kind {
        BrainEventKind::RunnerLeaseAcquired { lease } => {
            encode_runner_lease(builder.init_runner_lease_acquired(), lease);
        }
        BrainEventKind::RunnerLeaseReleased { lease_id } => {
            builder.set_runner_lease_released(&lease_id.0.to_string());
        }
        BrainEventKind::RunnerHandoffRequested { handoff } => {
            encode_runner_handoff(builder.init_runner_handoff_requested(), handoff);
        }
        BrainEventKind::RunnerHandoffCompleted { handoff_id, lease } => {
            let mut completed = builder.init_runner_handoff_completed();
            completed.set_handoff_id(&handoff_id.0.to_string());
            encode_runner_lease(completed.init_lease(), lease);
        }
        BrainEventKind::RunnerHandoffCancelled { handoff_id } => {
            builder.set_runner_handoff_cancelled(&handoff_id.0.to_string());
        }
        BrainEventKind::ClientAttached {
            attachment_id,
            connection_id,
            subject,
            role,
        } => {
            let mut attached = builder.init_client_attached();
            attached.set_attachment_id(&attachment_id.0.to_string());
            attached.set_connection_id(&connection_id.0.to_string());
            attached.set_subject(subject);
            attached.set_role(attachment_role_to_capnp(*role));
        }
        BrainEventKind::ClientDetached {
            attachment_id,
            connection_id,
        } => {
            let mut detached = builder.init_client_detached();
            detached.set_attachment_id(&attachment_id.0.to_string());
            detached.set_connection_id(&connection_id.0.to_string());
        }
        BrainEventKind::RunStarted { run } => encode_run(builder.init_run_started(), run),
        BrainEventKind::RunStatusChanged {
            run_id,
            status,
            detail,
        } => {
            let mut changed = builder.init_run_status_changed();
            changed.set_run_id(&run_id.0.to_string());
            changed.set_status(run_status_to_capnp(*status));
            if let Some(detail) = detail {
                changed.set_has_detail(true);
                changed.set_detail(detail);
            }
        }
        BrainEventKind::Prompt { text } => builder.set_prompt(text),
        BrainEventKind::ParticipantMessage { text } => builder.set_participant_message(text),
        BrainEventKind::ToolCall {
            request_seq,
            tool_id,
            name,
            input,
        } => {
            let mut call = builder.init_tool_call();
            call.set_request_seq(*request_seq);
            call.set_tool_id(tool_id);
            call.set_name(name);
            encode_json_value(call.reborrow().init_input(), input)?;
        }
        BrainEventKind::ToolResult {
            request_seq,
            tool_id,
            output,
            is_error,
        } => {
            let mut result = builder.init_tool_result();
            result.set_request_seq(*request_seq);
            result.set_tool_id(tool_id);
            result.set_output(output);
            result.set_is_error(*is_error);
        }
        BrainEventKind::ApprovalRequested {
            request_seq,
            approval_id,
            approval_kind,
            subject,
            audience,
            detail,
        } => {
            let mut requested = builder.init_approval_requested();
            requested.set_request_seq(*request_seq);
            requested.set_approval_id(approval_id);
            requested.set_approval_kind(approval_kind);
            requested.set_subject(subject);
            encode_json_value(requested.reborrow().init_detail(), detail)?;
            if let Some(audience) = audience {
                requested.set_has_audience(true);
                encode_approval_audience(requested.init_audience(), audience);
            }
        }
        BrainEventKind::ApprovalDecided {
            request_seq,
            approval_id,
            decision,
        } => {
            let mut decided = builder.init_approval_decided();
            decided.set_request_seq(*request_seq);
            decided.set_approval_id(approval_id);
            encode_json_value(decided.reborrow().init_decision(), decision)?;
        }
        BrainEventKind::Program { language, source } => {
            let mut program = builder.init_program();
            program.set_language(language_to_capnp(*language));
            program.set_source(source);
        }
        BrainEventKind::ProgramPopped { program_seq } => {
            builder.set_program_popped(*program_seq);
        }
        BrainEventKind::Result {
            request_seq,
            output,
            error,
        } => {
            let mut result = builder.init_result();
            result.set_request_seq(*request_seq);
            result.set_output(output);
            if let Some(error) = error {
                result.set_has_error(true);
                result.set_error(error);
            }
        }
        BrainEventKind::RuntimeCommitted {
            request_seq,
            runtime_revision,
            checkpoint_sha256,
        } => {
            let mut committed = builder.init_runtime_committed();
            committed.set_request_seq(*request_seq);
            committed.set_runtime_revision(*runtime_revision);
            committed.set_checkpoint_sha256(checkpoint_sha256);
        }
        BrainEventKind::EffectRecorded {
            request_seq,
            execution_id,
            effect,
            state,
        } => {
            let mut recorded = builder.init_effect_recorded();
            recorded.set_request_seq(*request_seq);
            recorded.set_execution_id(&execution_id.to_string());
            crate::ipc::checkpoint_codec::encode_vm_side_effect(
                recorded.reborrow().init_effect(),
                effect,
            )?;
            crate::ipc::checkpoint_codec::encode_effect_journal_state(
                recorded.reborrow().init_state(),
                state,
            )?;
        }
        BrainEventKind::ScheduleChanged { schedule } => {
            encode_schedule(builder.init_schedule_changed(), schedule);
        }
        BrainEventKind::ScheduleDue { due } => {
            encode_schedule_due(builder.init_schedule_due(), due);
        }
    }
    Ok(())
}

pub(super) fn decode_event(
    reader: finch_ipc_capnp::brain_event::Reader<'_>,
) -> anyhow::Result<BrainEvent> {
    use finch_ipc_capnp::brain_event::Which;
    let kind = match reader.which()? {
        Which::RunnerLeaseAcquired(lease) => BrainEventKind::RunnerLeaseAcquired {
            lease: decode_runner_lease(lease?)?,
        },
        Which::RunnerLeaseReleased(lease_id) => BrainEventKind::RunnerLeaseReleased {
            lease_id: RunnerLeaseId(parse_uuid(lease_id?)?),
        },
        Which::RunnerHandoffRequested(handoff) => BrainEventKind::RunnerHandoffRequested {
            handoff: decode_runner_handoff(handoff?)?,
        },
        Which::RunnerHandoffCompleted(completed) => {
            let completed = completed?;
            BrainEventKind::RunnerHandoffCompleted {
                handoff_id: RunnerHandoffId(parse_uuid(completed.get_handoff_id()?)?),
                lease: decode_runner_lease(completed.get_lease()?)?,
            }
        }
        Which::RunnerHandoffCancelled(handoff_id) => BrainEventKind::RunnerHandoffCancelled {
            handoff_id: RunnerHandoffId(parse_uuid(handoff_id?)?),
        },
        Which::ClientAttached(attached) => {
            let attached = attached?;
            BrainEventKind::ClientAttached {
                attachment_id: AttachmentId(parse_uuid(attached.get_attachment_id()?)?),
                connection_id: ConnectionId(parse_uuid(attached.get_connection_id()?)?),
                subject: text(attached.get_subject()?)?,
                role: attachment_role_from_capnp(attached.get_role()?),
            }
        }
        Which::ClientDetached(detached) => {
            let detached = detached?;
            BrainEventKind::ClientDetached {
                attachment_id: AttachmentId(parse_uuid(detached.get_attachment_id()?)?),
                connection_id: ConnectionId(parse_uuid(detached.get_connection_id()?)?),
            }
        }
        Which::RunStarted(run) => BrainEventKind::RunStarted {
            run: decode_run(run?)?,
        },
        Which::RunStatusChanged(changed) => {
            let changed = changed?;
            BrainEventKind::RunStatusChanged {
                run_id: RunId(parse_uuid(changed.get_run_id()?)?),
                status: run_status_from_capnp(changed.get_status()?),
                detail: changed
                    .get_has_detail()
                    .then(|| changed.get_detail())
                    .transpose()?
                    .map(text)
                    .transpose()?,
            }
        }
        Which::Prompt(value) => BrainEventKind::Prompt {
            text: text(value?)?,
        },
        Which::ParticipantMessage(value) => BrainEventKind::ParticipantMessage {
            text: text(value?)?,
        },
        Which::ToolCall(call) => {
            let call = call?;
            BrainEventKind::ToolCall {
                request_seq: call.get_request_seq(),
                tool_id: text(call.get_tool_id()?)?,
                name: text(call.get_name()?)?,
                input: decode_json_value(call.get_input()?)?,
            }
        }
        Which::ToolResult(result) => {
            let result = result?;
            BrainEventKind::ToolResult {
                request_seq: result.get_request_seq(),
                tool_id: text(result.get_tool_id()?)?,
                output: text(result.get_output()?)?,
                is_error: result.get_is_error(),
            }
        }
        Which::ApprovalRequested(requested) => {
            let requested = requested?;
            BrainEventKind::ApprovalRequested {
                request_seq: requested.get_request_seq(),
                approval_id: text(requested.get_approval_id()?)?,
                approval_kind: text(requested.get_approval_kind()?)?,
                subject: text(requested.get_subject()?)?,
                audience: requested
                    .get_has_audience()
                    .then(|| requested.get_audience())
                    .transpose()?
                    .map(decode_approval_audience)
                    .transpose()?,
                detail: decode_json_value(requested.get_detail()?)?,
            }
        }
        Which::ApprovalDecided(decided) => {
            let decided = decided?;
            BrainEventKind::ApprovalDecided {
                request_seq: decided.get_request_seq(),
                approval_id: text(decided.get_approval_id()?)?,
                decision: decode_json_value(decided.get_decision()?)?,
            }
        }
        Which::Program(program) => {
            let program = program?;
            BrainEventKind::Program {
                language: language_from_capnp(program.get_language()?),
                source: text(program.get_source()?)?,
            }
        }
        Which::ProgramPopped(program_seq) => BrainEventKind::ProgramPopped { program_seq },
        Which::Result(result) => {
            let result = result?;
            BrainEventKind::Result {
                request_seq: result.get_request_seq(),
                output: text(result.get_output()?)?,
                error: result
                    .get_has_error()
                    .then(|| result.get_error())
                    .transpose()?
                    .map(text)
                    .transpose()?,
            }
        }
        Which::RuntimeCommitted(committed) => {
            let committed = committed?;
            BrainEventKind::RuntimeCommitted {
                request_seq: committed.get_request_seq(),
                runtime_revision: committed.get_runtime_revision(),
                checkpoint_sha256: text(committed.get_checkpoint_sha256()?)?,
            }
        }
        Which::EffectRecorded(recorded) => {
            let recorded = recorded?;
            BrainEventKind::EffectRecorded {
                request_seq: recorded.get_request_seq(),
                execution_id: parse_uuid(recorded.get_execution_id()?)?,
                effect: crate::ipc::checkpoint_codec::decode_vm_side_effect(
                    recorded.get_effect()?,
                )?,
                state: crate::ipc::checkpoint_codec::decode_effect_journal_state(
                    recorded.get_state()?,
                )?,
            }
        }
        Which::ScheduleChanged(schedule) => BrainEventKind::ScheduleChanged {
            schedule: decode_schedule(schedule?)?,
        },
        Which::ScheduleDue(due) => BrainEventKind::ScheduleDue {
            due: decode_schedule_due(due?)?,
        },
    };
    Ok(BrainEvent {
        schema_version: reader.get_schema_version(),
        brain_id: parse_brain_id(reader.get_brain_id()?)?,
        seq: reader.get_seq(),
        environment_generation: reader.get_environment_generation(),
        sender: text(reader.get_sender()?)?,
        created_ms: reader.get_created_ms(),
        kind,
    })
}

fn parse_brain_id(reader: capnp::text::Reader<'_>) -> anyhow::Result<BrainId> {
    Ok(BrainId(parse_uuid(reader)?))
}

fn parse_uuid(reader: capnp::text::Reader<'_>) -> anyhow::Result<uuid::Uuid> {
    Ok(uuid::Uuid::parse_str(reader.to_str()?)?)
}

fn text(reader: capnp::text::Reader<'_>) -> anyhow::Result<String> {
    Ok(reader.to_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(brain_id: BrainId, seq: u64, kind: BrainEventKind) -> BrainEvent {
        BrainEvent {
            schema_version: 2,
            brain_id,
            seq,
            environment_generation: 7,
            sender: "alice@laptop.local".into(),
            created_ms: 123_000 + seq,
            kind,
        }
    }

    #[test]
    fn schema_native_dynamic_values_preserve_json_types() {
        let expected = serde_json::json!({
            "null": null,
            "bool": true,
            "signed": -17,
            "unsigned": u64::MAX,
            "float": 1.25,
            "text": "hello",
            "nested": [1, {"answer": 42}],
        });
        let mut message = capnp::message::Builder::new_default();
        encode_json_value(
            message.init_root::<finch_ipc_capnp::json_value::Builder<'_>>(),
            &expected,
        )
        .unwrap();

        let encoded = capnp::serialize::write_message_to_words(&message);
        let mut cursor = std::io::Cursor::new(encoded);
        let decoded = capnp::serialize::read_message(
            &mut cursor,
            capnp::message::ReaderOptions::new(),
        )
        .unwrap();
        let root = decoded
            .get_root::<finch_ipc_capnp::json_value::Reader<'_>>()
            .unwrap();

        assert_eq!(decode_json_value(root).unwrap(), expected);
    }

    #[test]
    fn typed_message_context_preserves_tool_inputs() {
        let expected_input = serde_json::json!({
            "path": "src/lib.rs",
            "line": 42,
            "flags": [true, false],
        });
        let messages = vec![crate::claude::Message {
            role: "assistant".into(),
            content: vec![
                crate::claude::ContentBlock::Text {
                    text: "checking".into(),
                },
                crate::claude::ContentBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "read".into(),
                    input: expected_input.clone(),
                },
                crate::claude::ContentBlock::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: "contents".into(),
                    is_error: Some(false),
                },
            ],
        }];
        let mut message = capnp::message::Builder::new_default();
        {
            let mut request =
                message.init_root::<finch_ipc_capnp::brain_turn_request::Builder<'_>>();
            encode_messages(
                request.reborrow().init_context(messages.len() as u32),
                &messages,
            )
            .unwrap();
        }

        let encoded = capnp::serialize::write_message_to_words(&message);
        let mut cursor = std::io::Cursor::new(encoded);
        let decoded = capnp::serialize::read_message(
            &mut cursor,
            capnp::message::ReaderOptions::new(),
        )
        .unwrap();
        let request = decoded
            .get_root::<finch_ipc_capnp::brain_turn_request::Reader<'_>>()
            .unwrap();
        let decoded = decode_messages(request.get_context().unwrap()).unwrap();

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].role, "assistant");
        assert!(matches!(
            &decoded[0].content[0],
            crate::claude::ContentBlock::Text { text } if text == "checking"
        ));
        assert!(matches!(
            &decoded[0].content[1],
            crate::claude::ContentBlock::ToolUse { id, name, input }
                if id == "tool-1" && name == "read" && input == &expected_input
        ));
        assert!(matches!(
            &decoded[0].content[2],
            crate::claude::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: Some(false),
            } if tool_use_id == "tool-1" && content == "contents"
        ));
    }

    #[test]
    fn participant_submission_union_round_trips_only_client_intent() {
        let submissions = vec![
            BrainEventKind::Prompt {
                text: "inspect the workspace".into(),
            },
            BrainEventKind::ParticipantMessage {
                text: "hello, collaborators".into(),
            },
            BrainEventKind::Program {
                language: ProgramLanguage::Lisp,
                source: "(say \"hello\")".into(),
            },
            BrainEventKind::ProgramPopped { program_seq: 41 },
            BrainEventKind::ApprovalDecided {
                request_seq: 17,
                approval_id: "approval-1".into(),
                decision: serde_json::json!({"kind": "allow_once"}),
            },
        ];

        for expected in submissions {
            let mut message = capnp::message::Builder::new_default();
            encode_brain_submission(
                message.init_root::<finch_ipc_capnp::brain_submission::Builder<'_>>(),
                &expected,
            )
            .unwrap();
            let encoded = capnp::serialize::write_message_to_words(&message);
            let mut cursor = std::io::Cursor::new(encoded);
            let decoded = capnp::serialize::read_message(
                &mut cursor,
                capnp::message::ReaderOptions::new(),
            )
            .unwrap();
            let root = decoded
                .get_root::<finch_ipc_capnp::brain_submission::Reader<'_>>()
                .unwrap();
            assert_eq!(decode_brain_submission(root).unwrap(), expected);
        }

        let mut message = capnp::message::Builder::new_default();
        let builder = message.init_root::<finch_ipc_capnp::brain_submission::Builder<'_>>();
        assert!(encode_brain_submission(
            builder,
            &BrainEventKind::Result {
                request_seq: 1,
                output: "forged".into(),
                error: None,
            },
        )
        .is_err());
    }

    #[test]
    fn every_current_brain_event_round_trips_through_capnp() {
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let connection_id = ConnectionId(uuid::Uuid::new_v4());
        let lease = BrainRunnerLease {
            lease_id: RunnerLeaseId(uuid::Uuid::new_v4()),
            subject: "runner@box.local".into(),
            environment_generation: 7,
            acquired_ms: 10,
            expires_ms: 20,
        };
        let audience = BrainApprovalAudience {
            brain_id,
            brain: "shared".into(),
            attachment_id,
            subject: "alice@laptop.local".into(),
            role: AttachmentRole::Driver,
            environment_generation: 7,
        };
        let kinds = vec![
            BrainEventKind::RunnerLeaseAcquired {
                lease: lease.clone(),
            },
            BrainEventKind::RunnerLeaseReleased {
                lease_id: lease.lease_id,
            },
            BrainEventKind::ClientAttached {
                attachment_id,
                connection_id,
                subject: "alice@laptop.local".into(),
                role: AttachmentRole::Driver,
            },
            BrainEventKind::ClientDetached {
                attachment_id,
                connection_id,
            },
            BrainEventKind::RunStarted {
                run: BrainRun {
                    run_id: RunId(uuid::Uuid::new_v4()),
                    kind: BrainRunKind::Interactive,
                    parent_run_id: None,
                    request_seq: 5,
                    initiating_attachment_id: attachment_id,
                    initiated_by: "alice@laptop.local".into(),
                    status: BrainRunStatus::Running,
                    started_ms: 100,
                    updated_ms: 100,
                    detail: None,
                },
            },
            BrainEventKind::RunStatusChanged {
                run_id: RunId(uuid::Uuid::new_v4()),
                status: BrainRunStatus::Interrupted,
                detail: Some("runner disconnected".into()),
            },
            BrainEventKind::Prompt {
                text: "inspect it".into(),
            },
            BrainEventKind::ParticipantMessage {
                text: "hello, collaborators".into(),
            },
            BrainEventKind::ToolCall {
                request_seq: 5,
                tool_id: "tool-1".into(),
                name: "read".into(),
                input: serde_json::json!({"path": "README.md"}),
            },
            BrainEventKind::ToolResult {
                request_seq: 5,
                tool_id: "tool-1".into(),
                output: "contents".into(),
                is_error: false,
            },
            BrainEventKind::ApprovalRequested {
                request_seq: 5,
                approval_id: "approval-1".into(),
                approval_kind: "tool".into(),
                subject: "edit".into(),
                audience: Some(audience),
                detail: serde_json::json!({"path": "src/lib.rs"}),
            },
            BrainEventKind::ApprovalDecided {
                request_seq: 5,
                approval_id: "approval-1".into(),
                decision: serde_json::json!({"choice": "approve_once"}),
            },
            BrainEventKind::Program {
                language: ProgramLanguage::Lisp,
                source: "(say \"done\")".into(),
            },
            BrainEventKind::ProgramPopped { program_seq: 9 },
            BrainEventKind::EffectRecorded {
                request_seq: 5,
                execution_id: uuid::Uuid::new_v4(),
                effect: crate::vm::VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 2,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    event: crate::vm::HostSideEffect::Emit {
                        text: "done".into(),
                    },
                    output: Vec::new(),
                    origin: crate::vm::SourceOrigin::generated("say"),
                },
                state: crate::vm::EffectJournalState::Acknowledged {
                    values: Vec::new(),
                },
            },
            BrainEventKind::Result {
                request_seq: 5,
                output: "done".into(),
                error: Some("example".into()),
            },
            BrainEventKind::RuntimeCommitted {
                request_seq: 5,
                runtime_revision: 3,
                checkpoint_sha256: "abc123".into(),
            },
        ];

        for (index, kind) in kinds.into_iter().enumerate() {
            let expected = BrainWireMessage::Event {
                event: event(brain_id, index as u64 + 1, kind),
            };
            let encoded = encode_brain_wire_message(&expected).unwrap();
            assert_eq!(decode_brain_wire_message(&encoded).unwrap(), expected);
        }
    }

    #[test]
    fn remote_brain_envelopes_round_trip_commands_and_correlated_replies() {
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment = BrainAttachment {
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: "alice@laptop.local".into(),
            role: AttachmentRole::Driver,
            acknowledged_seq: 4,
            connected: true,
            connection_id: Some(ConnectionId(uuid::Uuid::new_v4())),
        };
        let accepted = event(
            brain_id,
            5,
            BrainEventKind::Prompt {
                text: "inspect it".into(),
            },
        );
        let handoff = BrainRunnerHandoff {
            handoff_id: RunnerHandoffId(uuid::Uuid::new_v4()),
            from_lease_id: RunnerLeaseId(uuid::Uuid::new_v4()),
            requested_by: "alice@laptop.local".into(),
            target_subject: "runner-b@box.local".into(),
            environment_generation: 7,
            requested_ms: 100,
            expires_ms: 200,
        };
        let run = BrainRun {
            run_id: RunId(uuid::Uuid::new_v4()),
            kind: BrainRunKind::Interactive,
            parent_run_id: None,
            request_seq: 5,
            initiating_attachment_id: attachment.attachment_id,
            initiated_by: attachment.subject.clone(),
            status: BrainRunStatus::Cancelled,
            started_ms: 100,
            updated_ms: 200,
            detail: Some("cancelled by initiating driver".into()),
        };
        let schedule = BrainSchedule {
            schedule_id: ScheduleId(uuid::Uuid::new_v4()),
            initiating_attachment_id: attachment.attachment_id,
            created_by: attachment.subject.clone(),
            grant_ceiling: crate::vm::EffectSet::default(),
            language: ProgramLanguage::Lisp,
            source: "(say \"later\")".into(),
            next_due_ms: 500,
            interval_ms: Some(100),
            delivery_policy: BrainScheduleDeliveryPolicy::Coalesce,
            active: true,
        };
        let envelopes = vec![
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 1,
                kind: BrainRemoteCommandKind::Submit(BrainEventKind::Prompt {
                    text: "inspect it".into(),
                }),
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 2,
                kind: BrainRemoteCommandKind::Acknowledge(5),
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 3,
                kind: BrainRemoteCommandKind::Detach,
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 4,
                kind: BrainRemoteCommandKind::RequestRunnerHandoff {
                    target_subject: handoff.target_subject.clone(),
                    expected_lease_id: handoff.from_lease_id,
                    environment_generation: handoff.environment_generation,
                    ttl_ms: 30_000,
                },
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 5,
                kind: BrainRemoteCommandKind::CancelRunnerHandoff(handoff.handoff_id),
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 6,
                kind: BrainRemoteCommandKind::CancelRun(run.run_id),
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 7,
                kind: BrainRemoteCommandKind::CreateSchedule {
                    language: schedule.language,
                    source: schedule.source.clone(),
                    grant_ceiling: schedule.grant_ceiling.clone(),
                    next_due_ms: schedule.next_due_ms,
                    interval_ms: schedule.interval_ms,
                    delivery_policy: schedule.delivery_policy.clone(),
                },
            }),
            BrainRemoteEnvelope::Command(BrainRemoteCommand {
                request_id: 8,
                kind: BrainRemoteCommandKind::CancelSchedule(schedule.schedule_id),
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::Submitted {
                request_id: 1,
                accepted,
                run: None,
                result: None,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::Acknowledged {
                request_id: 2,
                attachment,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::Detached { request_id: 3 }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::HandoffRequested {
                request_id: 4,
                handoff,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::HandoffCancelled { request_id: 5 }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::RunCancelled {
                request_id: 6,
                run,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::ScheduleCreated {
                request_id: 7,
                schedule,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::ScheduleCancelled {
                request_id: 8,
                cancelled: true,
            }),
            BrainRemoteEnvelope::Reply(BrainRemoteReply::Error {
                request_id: 9,
                code: "forbidden".into(),
                message: "scope denied".into(),
            }),
        ];

        for expected in envelopes {
            let encoded = encode_brain_remote_envelope(&expected).unwrap();
            assert_eq!(decode_brain_remote_envelope(&encoded).unwrap(), expected);
        }
    }

    #[test]
    fn complete_brain_snapshot_round_trips_through_capnp() {
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let connection_id = ConnectionId(uuid::Uuid::new_v4());
        let lease = BrainRunnerLease {
            lease_id: RunnerLeaseId(uuid::Uuid::new_v4()),
            subject: "runner@box.local".into(),
            environment_generation: 7,
            acquired_ms: 10,
            expires_ms: 20,
        };
        let schedule_id = ScheduleId(uuid::Uuid::new_v4());
        let scheduled_run = BrainRun {
            run_id: RunId(uuid::Uuid::new_v4()),
            kind: BrainRunKind::Scheduled,
            parent_run_id: None,
            request_seq: 3,
            initiating_attachment_id: attachment_id,
            initiated_by: "alice@laptop.local".into(),
            status: BrainRunStatus::QueuedForEnvironment,
            started_ms: 300,
            updated_ms: 300,
            detail: None,
        };
        let schedule_grant_ceiling = crate::vm::EffectSet(
            [crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("reports/**").unwrap(),
            )]
            .into_iter()
            .collect(),
        );
        let schedule = BrainSchedule {
            schedule_id,
            initiating_attachment_id: attachment_id,
            created_by: "alice@laptop.local".into(),
            grant_ceiling: schedule_grant_ceiling.clone(),
            language: ProgramLanguage::Lisp,
            source: "(say \"scheduled\")".into(),
            next_due_ms: 500,
            interval_ms: Some(1_000),
            delivery_policy: BrainScheduleDeliveryPolicy::BoundedCatchUp {
                max_catch_up: 2,
                expires_after_ms: 60_000,
            },
            active: true,
        };
        let pending_due = BrainScheduleDue {
            schedule_id,
            run: scheduled_run.clone(),
            language: ProgramLanguage::Lisp,
            source: "(say \"scheduled\")".into(),
            grant_ceiling: schedule_grant_ceiling,
            due_at_ms: 500,
            first_missed_at_ms: 400,
            missed_count: 2,
            next_due_ms: Some(1_500),
        };
        let expected = BrainWireMessage::Snapshot {
            brain: BrainSnapshot {
                brain_id,
                name: "shared".into(),
                environment: BrainEnvironment {
                    machine: "box.local".into(),
                    workspace: "/workspace/project".into(),
                    generation: 7,
                },
                revision: 1,
                events: vec![event(
                    brain_id,
                    1,
                    BrainEventKind::Prompt {
                        text: "hello".into(),
                    },
                )],
                program_stack: vec![BrainProgram {
                    seq: 2,
                    sender: "provider".into(),
                    language: ProgramLanguage::Forth,
                    source: "\"hello\" say".into(),
                }],
                attachments: vec![BrainAttachment {
                    attachment_id,
                    subject: "alice@laptop.local".into(),
                    role: AttachmentRole::Driver,
                    acknowledged_seq: 1,
                    connected: true,
                    connection_id: Some(connection_id),
                }],
                runner_lease: Some(lease),
                runner_handoff: None,
                runs: vec![BrainRun {
                    run_id: RunId(uuid::Uuid::new_v4()),
                    kind: BrainRunKind::Interactive,
                    parent_run_id: None,
                    request_seq: 1,
                    initiating_attachment_id: attachment_id,
                    initiated_by: "alice@laptop.local".into(),
                    status: BrainRunStatus::Completed,
                    started_ms: 100,
                    updated_ms: 200,
                    detail: None,
                }, scheduled_run],
                schedules: vec![schedule],
                pending_schedule_dues: vec![pending_due],
            },
        };
        let encoded = encode_brain_wire_message(&expected).unwrap();
        assert_eq!(decode_brain_wire_message(&encoded).unwrap(), expected);
    }
}
