use crate::brain::shared::{
    AttachmentId, AttachmentRole, BrainApprovalAudience, BrainAttachment, BrainEnvironment,
    BrainEvent, BrainEventKind, BrainId, BrainProgram, BrainRun, BrainRunKind, BrainRunStatus,
    BrainRunnerLease, BrainSnapshot, BrainWireMessage, ConnectionId, ProgramLanguage, RunId,
    RunnerLeaseId,
};
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
            decided.set_decision_json(&serde_json::to_vec(decision)?);
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
                decision: serde_json::from_slice(decided.get_decision_json()?)?,
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

pub(crate) fn encode_brain_wire_message(message: &BrainWireMessage) -> anyhow::Result<Vec<u8>> {
    let mut encoded = capnp::message::Builder::new_default();
    let root = encoded.init_root::<finch_ipc_capnp::brain_wire_message::Builder<'_>>();
    match message {
        BrainWireMessage::Snapshot { brain } => encode_snapshot(root.init_snapshot(), brain)?,
        BrainWireMessage::Event { event } => encode_event(root.init_event(), event)?,
    }
    Ok(capnp::serialize::write_message_to_words(&encoded))
}

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
    let mut runs = builder.reborrow().init_runs(snapshot.runs.len() as u32);
    for (index, run) in snapshot.runs.iter().enumerate() {
        encode_run(runs.reborrow().get(index as u32), run);
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
        runs,
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
            call.set_input_json(&serde_json::to_vec(input)?);
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
            requested.set_detail_json(&serde_json::to_vec(detail)?);
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
            decided.set_decision_json(&serde_json::to_vec(decision)?);
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
        Which::ToolCall(call) => {
            let call = call?;
            BrainEventKind::ToolCall {
                request_seq: call.get_request_seq(),
                tool_id: text(call.get_tool_id()?)?,
                name: text(call.get_name()?)?,
                input: serde_json::from_slice(call.get_input_json()?)?,
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
                detail: serde_json::from_slice(requested.get_detail_json()?)?,
            }
        }
        Which::ApprovalDecided(decided) => {
            let decided = decided?;
            BrainEventKind::ApprovalDecided {
                request_seq: decided.get_request_seq(),
                approval_id: text(decided.get_approval_id()?)?,
                decision: serde_json::from_slice(decided.get_decision_json()?)?,
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
    fn participant_submission_union_round_trips_only_client_intent() {
        let submissions = vec![
            BrainEventKind::Prompt {
                text: "inspect the workspace".into(),
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
                }],
            },
        };
        let encoded = encode_brain_wire_message(&expected).unwrap();
        assert_eq!(decode_brain_wire_message(&encoded).unwrap(), expected);
    }
}
