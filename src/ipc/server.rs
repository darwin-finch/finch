//! Cap'n Proto RPC server — runs inside the daemon, listens on the Unix socket.
//!
//! Each inbound connection gets its own `FinchDaemonImpl` backed by the
//! shared `Arc<AgentServer>`.

use std::sync::Arc;

use anyhow::Result;
use capnp::capability::Promise;
use capnp_rpc::{pry, rpc_twoparty_capnp, twoparty, RpcSystem};
use tokio::net::UnixListener;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::ipc::brain_codec::{
    decode_approval_audience, decode_brain_submission, decode_environment, encode_approval_audience,
    encode_attachment, encode_brain_submission_outcome, encode_event, encode_runner_lease,
    encode_snapshot,
};
use crate::ipc::schema::finch_ipc_capnp::{self, brain_service, finch_daemon};
use crate::server::AgentServer;

// ---------------------------------------------------------------------------
// Server implementation struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FinchDaemonImpl {
    server: Arc<AgentServer>,
}

impl FinchDaemonImpl {
    fn new(server: Arc<AgentServer>) -> Self {
        Self { server }
    }
}

#[derive(Clone)]
struct BrainServiceImpl {
    server: Arc<AgentServer>,
}

/// Reverse per-turn capability used by the leased frontend runner to suspend
/// on an approval without deciding it locally. The daemon records the request
/// and resumes it only from the attachment named by `expected_audience`.
struct BrainTurnControlImpl {
    server: Arc<AgentServer>,
    brain: String,
    request_seq: u64,
    expected_audience: crate::brain::shared::BrainApprovalAudience,
}

impl finch_ipc_capnp::brain_turn_control::Server for BrainTurnControlImpl {
    fn request_approval(
        &mut self,
        params: finch_ipc_capnp::brain_turn_control::RequestApprovalParams,
        mut results: finch_ipc_capnp::brain_turn_control::RequestApprovalResults,
    ) -> Promise<(), capnp::Error> {
        let encoded = match params.get().and_then(|params| params.get_event()) {
            Ok(encoded) => encoded,
            Err(error) => return Promise::err(error),
        };
        let event = match decode_runner_turn_event(encoded) {
            Ok(event) => event,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let crate::server::RunnerTurnEvent::ApprovalRequested {
            approval_id,
            approval_kind,
            subject,
            audience,
            detail,
        } = event
        else {
            return Promise::err(capnp::Error::failed(
                "Brain turn control accepts only approval requests".into(),
            ));
        };
        if audience != self.expected_audience {
            return Promise::err(capnp::Error::failed(format!(
                "runner substituted the approval audience for request {}",
                self.request_seq
            )));
        }

        let registration = match self.server.brain_approvals().register(
            self.request_seq,
            approval_id.clone(),
            audience.clone(),
        ) {
            Ok(registration) => registration,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let run_id = match self.server.shared_brains().snapshot(&self.brain) {
            Ok(snapshot) => snapshot
                .runs
                .into_iter()
                .find(|run| {
                    run.request_seq == self.request_seq
                        && run.status == crate::brain::shared::BrainRunStatus::Running
                })
                .map(|run| run.run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if let Some(run_id) = run_id {
            if let Err(error) = self.server.shared_brains().transition_run(
                &self.brain,
                "daemon",
                run_id,
                crate::brain::shared::BrainRunStatus::AwaitingApproval,
                Some(format!("awaiting approval {approval_id}")),
            ) {
                return Promise::err(capnp::Error::failed(error.to_string()));
            }
        }
        if approval_kind == "tool" {
            let input = detail
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if let Err(error) = self.server.shared_brains().push(
                &self.brain,
                "provider",
                crate::brain::shared::BrainEventKind::ToolCall {
                    request_seq: self.request_seq,
                    tool_id: approval_id.clone(),
                    name: subject.clone(),
                    input,
                },
            ) {
                if let Some(run_id) = run_id {
                    let _ = self.server.shared_brains().transition_run(
                        &self.brain,
                        "daemon",
                        run_id,
                        crate::brain::shared::BrainRunStatus::Interrupted,
                        Some(error.to_string()),
                    );
                }
                return Promise::err(capnp::Error::failed(error.to_string()));
            }
        }
        if let Err(error) = self.server.shared_brains().push(
            &self.brain,
            "runner",
            crate::brain::shared::BrainEventKind::ApprovalRequested {
                request_seq: self.request_seq,
                approval_id,
                approval_kind,
                subject,
                audience: Some(audience),
                detail,
            },
        ) {
            if let Some(run_id) = run_id {
                let _ = self.server.shared_brains().transition_run(
                    &self.brain,
                    "daemon",
                    run_id,
                    crate::brain::shared::BrainRunStatus::Interrupted,
                    Some(error.to_string()),
                );
            }
            return Promise::err(capnp::Error::failed(error.to_string()));
        }

        let store = self.server.shared_brains().clone();
        let brain = self.brain.clone();
        Promise::from_future(async move {
            let decision = match registration.wait().await {
                Ok(decision) => {
                    if let Some(run_id) = run_id {
                        store
                            .transition_run(
                                &brain,
                                "daemon",
                                run_id,
                                crate::brain::shared::BrainRunStatus::Running,
                                None,
                            )
                            .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    }
                    decision
                }
                Err(error) => {
                    if let Some(run_id) = run_id {
                        let _ = store.transition_run(
                            &brain,
                            "daemon",
                            run_id,
                            crate::brain::shared::BrainRunStatus::Interrupted,
                            Some(error.to_string()),
                        );
                    }
                    return Err(capnp::Error::failed(error.to_string()));
                }
            };
            let decision = serde_json::to_vec(&decision)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_decision_json(&decision);
            Ok(())
        })
    }
}

impl brain_service::Server for BrainServiceImpl {
    fn snapshot(
        &mut self,
        params: brain_service::SnapshotParams,
        mut results: brain_service::SnapshotResults,
    ) -> Promise<(), capnp::Error> {
        let brain = pry!(pry!(params.get()).get_brain())
            .to_str()
            .unwrap_or("")
            .to_string();
        let snapshot = match self.server.shared_brains().snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        match encode_snapshot(results.get().init_snapshot(), &snapshot) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn attach(
        &mut self,
        params: brain_service::AttachParams,
        mut results: brain_service::AttachResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let subject = pry!(params.get_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let role = match params.get_role() {
            Ok(role) => attachment_role_from_capnp(role),
            Err(error) => return Promise::err(error.into()),
        };
        if role == crate::brain::shared::AttachmentRole::Runner {
            return Promise::err(capnp::Error::failed(
                "runner authority requires a runner lease, not a client attachment".into(),
            ));
        }
        let attachment_id = if params.get_has_attachment_id() {
            match parse_attachment_id(params.get_attachment_id()) {
                Ok(id) => Some(id),
                Err(error) => return Promise::err(error),
            }
        } else {
            None
        };
        let attachment = match self
            .server
            .shared_brains()
            .attach(&brain, &subject, role, attachment_id)
        {
            Ok(attachment) => attachment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let store = self.server.shared_brains().clone();
        let pending_brain = brain.clone();
        let pending_attachment_id = attachment.attachment_id;
        let pending_connection_id = attachment
            .connection_id
            .expect("new Brain attachment has a pending connection");
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if store
                .expire_pending_connection(
                    &pending_brain,
                    pending_attachment_id,
                    pending_connection_id,
                )
                .unwrap_or(false)
            {
                let _ = store.remove_if_unused(&pending_brain);
            }
        });
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn acknowledge(
        &mut self,
        params: brain_service::AcknowledgeParams,
        mut results: brain_service::AcknowledgeResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let attachment = match self.server.shared_brains().acknowledge(
            &brain,
            attachment_id,
            connection_id,
            params.get_seq(),
        ) {
            Ok(attachment) => attachment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn detach(
        &mut self,
        params: brain_service::DetachParams,
        _results: brain_service::DetachResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let snapshot = match self.server.shared_brains().snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if let Err(error) = self
            .server
            .shared_brains()
            .detach(&brain, attachment_id, connection_id)
        {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        self.server
            .brain_approvals()
            .cancel_attachment(snapshot.brain_id, attachment_id);
        let _ = self.server.shared_brains().remove_if_unused(&brain);
        Promise::ok(())
    }

    fn submit(
        &mut self,
        params: brain_service::SubmitParams,
        mut results: brain_service::SubmitResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let kind = match params
            .get_submission()
            .map_err(anyhow::Error::from)
            .and_then(decode_brain_submission)
        {
            Ok(kind) => kind,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let attachment = match self.server.shared_brains().require_connection(
            &brain,
            attachment_id,
            connection_id,
        ) {
            Ok(attachment) => attachment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let store = self.server.shared_brains().clone();
        let runners = self.server.brain_runners().clone();
        let approvals = self.server.brain_approvals().clone();
        Promise::from_future(async move {
            let outcome = crate::server::handlers::submit_named_brain_event(
                &store,
                &runners,
                &approvals,
                &brain,
                &attachment,
                kind,
            )
            .await
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encode_brain_submission_outcome(
                results.get().init_outcome(),
                &outcome.accepted,
                outcome.run.as_ref(),
                outcome.result.as_ref(),
            )
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
            Ok(())
        })
    }

    fn watch(
        &mut self,
        params: brain_service::WatchParams,
        _results: brain_service::WatchResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let receiver = pry!(params.get_receiver());
        if let Err(error) = self.server.shared_brains().require_connection(
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        if let Err(error) = self.server.shared_brains().activate_connection(
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let mut events = match self.server.shared_brains().subscribe(&brain) {
            Ok(events) => events,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let snapshot = match self.server.shared_brains().snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let brain_id = snapshot.brain_id;
        let store = self.server.shared_brains().clone();
        let approvals = self.server.brain_approvals().clone();
        Promise::from_future(async move {
            let mut initial = receiver.on_message_request();
            let initial_result = encode_snapshot(
                initial.get().init_message().init_snapshot(),
                &snapshot,
            )
            .map_err(|error| capnp::Error::failed(error.to_string()))
            .and_then(|()| Ok(initial));
            let initial_error = match initial_result {
                Ok(initial) => initial.send().promise.await.err(),
                Err(error) => Some(error),
            };
            if let Some(error) = initial_error {
                let _ = store.detach(&brain, attachment_id, connection_id);
                approvals.cancel_attachment(brain_id, attachment_id);
                let _ = store.remove_if_unused(&brain);
                return Err(error);
            }
            let watch_error = loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(error) => break Some(capnp::Error::failed(error.to_string())),
                };
                if event.seq <= snapshot.revision {
                    continue;
                }
                let mut call = receiver.on_message_request();
                encode_event(call.get().init_message().init_event(), &event)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                if call.send().promise.await.is_err() {
                    break None;
                }
            };
            let _ = store.detach(&brain, attachment_id, connection_id);
            approvals.cancel_attachment(brain_id, attachment_id);
            let _ = store.remove_if_unused(&brain);
            watch_error.map_or(Ok(()), Err)
        })
    }

    fn acquire_runner(
        &mut self,
        params: brain_service::AcquireRunnerParams,
        mut results: brain_service::AcquireRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let subject = pry!(params.get_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let environment = match params
            .get_environment()
            .map_err(anyhow::Error::from)
            .and_then(decode_environment)
        {
            Ok(environment) => environment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if &environment != self.server.shared_brains().environment() {
            return Promise::err(capnp::Error::failed(
                "runner environment does not match the daemon Brain environment".into(),
            ));
        }
        let lease_id = if params.get_has_lease_id() {
            match parse_runner_lease_id(params.get_lease_id()) {
                Ok(id) => Some(id),
                Err(error) => return Promise::err(error),
            }
        } else {
            None
        };
        let lease = match self.server.shared_brains().acquire_runner_lease(
            &brain,
            &subject,
            environment.generation,
            lease_id,
            params.get_ttl_ms(),
        ) {
            Ok(lease) => lease,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let store = self.server.shared_brains().clone();
        let lease_brain = brain.clone();
        let lease_id = lease.lease_id;
        let expires_ms = lease.expires_ms;
        tokio::spawn(async move {
            loop {
                let delay_ms = expires_ms.saturating_sub(unix_epoch_millis());
                if delay_ms == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if store
                .expire_runner_lease(&lease_brain, lease_id, unix_epoch_millis())
                .is_ok_and(|expired| expired)
            {
                let _ = store.remove_if_unused(&lease_brain);
            }
        });
        encode_runner_lease(results.get().init_lease(), &lease);
        Promise::ok(())
    }

    fn release_runner(
        &mut self,
        params: brain_service::ReleaseRunnerParams,
        _results: brain_service::ReleaseRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let lease_id = match parse_runner_lease_id(params.get_lease_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self
            .server
            .shared_brains()
            .release_runner_lease(&brain, lease_id)
        {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let _ = self.server.shared_brains().remove_if_unused(&brain);
        Promise::ok(())
    }
}

fn parse_attachment_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::shared::AttachmentId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::shared::AttachmentId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_connection_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::shared::ConnectionId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::shared::ConnectionId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_runner_lease_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::shared::RunnerLeaseId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::shared::RunnerLeaseId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn unix_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Helper: read a capnp Message list into internal Message vec
// ---------------------------------------------------------------------------

fn read_messages(
    list: capnp::struct_list::Reader<finch_ipc_capnp::message::Owned>,
) -> Result<Vec<crate::claude::Message>, capnp::Error> {
    let mut out = Vec::with_capacity(list.len() as usize);
    for msg in list.iter() {
        let role = msg.get_role()?.to_str()?.to_string();
        let mut content = Vec::new();
        for block in msg.get_content()?.iter() {
            use finch_ipc_capnp::content_block::Which;
            match block.which()? {
                Which::Text(t) => {
                    content.push(crate::claude::ContentBlock::Text {
                        text: t?.to_str()?.to_string(),
                    });
                }
                Which::ToolUse(tu) => {
                    let tu = tu?;
                    let input: serde_json::Value =
                        serde_json::from_str(tu.get_input_json()?.to_str()?)
                            .unwrap_or(serde_json::Value::Null);
                    content.push(crate::claude::ContentBlock::ToolUse {
                        id: tu.get_id()?.to_str()?.to_string(),
                        name: tu.get_name()?.to_str()?.to_string(),
                        input,
                    });
                }
                Which::ToolResult(tr) => {
                    let tr = tr?;
                    content.push(crate::claude::ContentBlock::ToolResult {
                        tool_use_id: tr.get_tool_use_id()?.to_str()?.to_string(),
                        content: tr.get_content()?.to_str()?.to_string(),
                        is_error: Some(tr.get_is_error()),
                    });
                }
                Which::Thinking(t) => {
                    // Ignore thinking blocks on ingestion (no internal type for it yet)
                    let _ = t;
                }
            }
        }
        out.push(crate::claude::Message { role, content });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helper: read tool definitions
// ---------------------------------------------------------------------------

fn read_tools(
    list: capnp::struct_list::Reader<finch_ipc_capnp::tool_definition::Owned>,
) -> Result<Vec<crate::tools::types::ToolDefinition>, capnp::Error> {
    let mut out = Vec::with_capacity(list.len() as usize);
    for td in list.iter() {
        let schema: crate::tools::types::ToolInputSchema =
            serde_json::from_str(td.get_input_schema_json()?.to_str()?).unwrap_or_else(|_| {
                crate::tools::types::ToolInputSchema {
                    schema_type: "object".to_string(),
                    properties: serde_json::Value::Object(serde_json::Map::new()),
                    required: vec![],
                }
            });
        out.push(crate::tools::types::ToolDefinition {
            name: td.get_name()?.to_str()?.to_string(),
            description: td.get_description()?.to_str()?.to_string(),
            input_schema: schema,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helper: write QueryResponse into capnp builder
// ---------------------------------------------------------------------------

fn write_query_response(
    mut builder: finch_ipc_capnp::query_response::Builder,
    text: &str,
    tool_uses: &[crate::tools::types::ToolUse],
    model: &str,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    latency_ms: Option<u64>,
) {
    builder.set_text(text);
    builder.set_model(model);
    builder.set_input_tokens(input_tokens.unwrap_or(0));
    builder.set_output_tokens(output_tokens.unwrap_or(0));
    builder.set_latency_ms(latency_ms.unwrap_or(0));

    let mut tu_list = builder.init_tool_uses(tool_uses.len() as u32);
    for (i, tu) in tool_uses.iter().enumerate() {
        let mut t = tu_list.reborrow().get(i as u32);
        t.set_id(tu.id.as_str());
        t.set_name(tu.name.as_str());
        t.set_input_json(tu.input.to_string().as_str());
    }
}

async fn execute_typed_forth_ipc(program: String) -> Result<(Vec<i64>, String)> {
    let runtime = crate::runtime::ProgramRuntime::new();
    runtime.grant_typed_capability(crate::vm::CapabilityRequirement {
        capability: crate::vm::CapabilityKind::SessionEmit,
        selector: crate::vm::ResourceSelector::None,
    })?;
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("capnp:evalForth".to_string()),
            source: program,
            intent: "execute typed Co-Forth over the local IPC boundary".to_string(),
            effect: crate::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: None,
            budget: None,
        })
        .await?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
        let diagnostic = outcome
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| format!("typed Co-Forth ended as {:?}", outcome.status));
        anyhow::bail!(diagnostic);
    }
    let stack = outcome
        .values
        .iter()
        .map(|value| match value {
            crate::programs::ProgramValue::Int(value) => Ok(*value),
            other => {
                anyhow::bail!("evalForth IPC supports only integer stack results; found {other:?}")
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((stack, outcome.output))
}

// ---------------------------------------------------------------------------
// RPC method implementations
// ---------------------------------------------------------------------------

impl finch_daemon::Server for FinchDaemonImpl {
    // ---- query (non-streaming) -------------------------------------------

    fn query(
        &mut self,
        params: finch_daemon::QueryParams,
        mut results: finch_daemon::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let messages = pry!(read_messages(pry!(p.get_messages())));
        let tools = pry!(read_tools(pry!(p.get_tools())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let provider = server
                .primary_provider()
                .ok_or_else(|| capnp::Error::failed("no provider configured".into()))?;

            let mut req = crate::providers::ProviderRequest::new(messages);
            if !tools.is_empty() {
                req = req.with_tools(tools);
            }

            let response = provider
                .send_message(&req)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))?;

            let tool_uses = response.tool_uses();
            write_query_response(
                results.get().init_response(),
                &response.text(),
                &tool_uses,
                &response.model,
                None,
                None,
                None,
            );
            Ok(())
        })
    }

    // ---- query_stream (streaming) ----------------------------------------

    fn query_stream(
        &mut self,
        params: finch_daemon::QueryStreamParams,
        _results: finch_daemon::QueryStreamResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let messages = pry!(read_messages(pry!(p.get_messages())));
        let tools = pry!(read_tools(pry!(p.get_tools())));
        let receiver = pry!(p.get_receiver());
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let provider = server
                .primary_provider()
                .ok_or_else(|| capnp::Error::failed("no provider configured".into()))?;

            let mut req = crate::providers::ProviderRequest::new(messages);
            if !tools.is_empty() {
                req = req.with_tools(tools);
            }

            if !provider.supports_streaming() {
                // Fall back to blocking send; emit one text chunk then done.
                let response = provider
                    .send_message(&req)
                    .await
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                let text = response.text();
                if !text.is_empty() {
                    let mut r = receiver.on_chunk_request();
                    r.get().init_chunk().set_text_delta(text.as_str());
                    r.send().promise.await?;
                }
                let mut r = receiver.on_chunk_request();
                r.get().init_chunk().set_done(());
                r.send().promise.await?;
                return Ok(());
            }

            let mut rx = provider
                .send_message_stream(&req)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))?;

            use crate::generators::StreamChunk;
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(StreamChunk::TextDelta(delta)) => {
                        let mut r = receiver.on_chunk_request();
                        r.get().init_chunk().set_text_delta(delta.as_str());
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::Usage { input_tokens }) => {
                        let mut r = receiver.on_chunk_request();
                        let mut upd = r.get().init_chunk().init_usage_update();
                        upd.set_input_tokens(input_tokens);
                        upd.set_output_tokens(0);
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::ContentBlockComplete(block)) => {
                        if let crate::claude::ContentBlock::ToolUse { id, name, input } = block {
                            let mut r = receiver.on_chunk_request();
                            let mut tu = r.get().init_chunk().init_tool_use_complete();
                            tu.set_id(id.as_str());
                            tu.set_name(name.as_str());
                            tu.set_input_json(input.to_string().as_str());
                            r.send().promise.await?;
                        }
                    }
                    Err(e) => {
                        let mut r = receiver.on_chunk_request();
                        r.get().init_chunk().set_error(e.to_string().as_str());
                        r.send().promise.await?;
                        return Ok(());
                    }
                }
            }

            // Done sentinel
            let mut r = receiver.on_chunk_request();
            r.get().init_chunk().set_done(());
            r.send().promise.await?;
            Ok(())
        })
    }

    // ---- Typed Co-Forth --------------------------------------------------

    fn eval_forth(
        &mut self,
        params: finch_daemon::EvalForthParams,
        mut results: finch_daemon::EvalForthResults,
    ) -> Promise<(), capnp::Error> {
        let program = pry!(pry!(params.get()).get_program())
            .to_str()
            .unwrap_or("")
            .to_owned();

        Promise::from_future(async move {
            let (stack, output) = execute_typed_forth_ipc(program)
                .await
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            let mut response = results.get();
            let mut list = response.reborrow().init_stack(stack.len() as u32);
            for (index, value) in stack.into_iter().enumerate() {
                list.set(index as u32, value);
            }
            response.reborrow().set_output(&output);
            response.set_error("");
            Ok(())
        })
    }

    fn register_brain_runner(
        &mut self,
        params: finch_daemon::RegisterBrainRunnerParams,
        mut results: finch_daemon::RegisterBrainRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let lease_text = pry!(params.get_lease_id())
            .to_str()
            .unwrap_or("")
            .to_string();
        let lease_uuid = match uuid::Uuid::parse_str(&lease_text) {
            Ok(value) => value,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let lease_id = crate::brain::shared::RunnerLeaseId(lease_uuid);
        let runner = pry!(params.get_runner());
        let snapshot = match self.server.shared_brains().snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if !snapshot
            .runner_lease
            .as_ref()
            .is_some_and(|lease| lease.lease_id == lease_id)
        {
            return Promise::err(capnp::Error::failed(
                "runner callback does not match the active lease".into(),
            ));
        }

        let (runtime_revision, checkpoint) =
            match self.server.shared_brains().runner_checkpoint(&brain) {
                Ok(checkpoint) => checkpoint,
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            };
        let checkpoint_json = match serde_json::to_vec(&checkpoint) {
            Ok(encoded) => encoded,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let broker = self.server.brain_runners().clone();
        let server = Arc::clone(&self.server);
        let registration_id = broker.register(brain.clone(), lease_id, tx);
        let queued_store = server.shared_brains().clone();
        let queued_broker = broker.clone();
        let queued_brain = brain.clone();
        tokio::task::spawn_local(async move {
            while let Some(request) = rx.recv().await {
                let disconnected = match request {
                    crate::server::RunnerRequest::Program(request) => {
                        let mut call = runner.run_program_request();
                        {
                            let mut payload = call.get().init_request();
                            payload.set_brain(&request.brain);
                            payload.set_request_seq(request.request_seq);
                            payload.set_language(program_language_to_capnp(request.language));
                            payload.set_source(&request.source);
                        }
                        let (result, disconnected) = match call.send().promise.await {
                            Ok(reply) => (
                                decode_runner_program_result(
                                    reply.get().and_then(|r| r.get_result()),
                                ),
                                false,
                            ),
                            Err(error) => (Err(error.to_string()), true),
                        };
                        let _ = request.response_tx.send(result);
                        disconnected
                    }
                    crate::server::RunnerRequest::Turn(request) => {
                        let context_json =
                            serde_json::to_vec(&request.context).map_err(|error| error.to_string());
                        let (result, disconnected) = match context_json {
                            Ok(context_json) => {
                                let mut call = runner.run_turn_request();
                                {
                                    let mut payload = call.get().init_request();
                                    payload.set_brain(&request.brain);
                                    payload.set_request_seq(request.request_seq);
                                    payload.set_prompt(&request.prompt);
                                    payload.set_context_json(&context_json);
                                    encode_approval_audience(
                                        payload.reborrow().init_approval_audience(),
                                        &request.approval_audience,
                                    );
                                    let control: finch_ipc_capnp::brain_turn_control::Client =
                                        capnp_rpc::new_client(BrainTurnControlImpl {
                                            server: Arc::clone(&server),
                                            brain: request.brain.clone(),
                                            request_seq: request.request_seq,
                                            expected_audience: request.approval_audience.clone(),
                                        });
                                    payload.set_control(control);
                                }
                                match call.send().promise.await {
                                    Ok(reply) => (
                                        decode_runner_turn_result(
                                            reply.get().and_then(|r| r.get_result()),
                                        ),
                                        false,
                                    ),
                                    Err(error) => (Err(error.to_string().into()), true),
                                }
                            }
                            Err(error) => (Err(error.into()), false),
                        };
                        let _ = request.response_tx.send(result);
                        disconnected
                    }
                };
                if disconnected {
                    break;
                }
            }
            broker.unregister(&brain, registration_id);
        });
        // Return the registration bootstrap first. The frontend then marks
        // this lease active before the queued callback reaches its event loop.
        tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            if let Err(error) = crate::server::handlers::resume_queued_named_brain_runs(
                queued_store,
                queued_broker,
                queued_brain.clone(),
                lease_id,
            )
            .await
            {
                tracing::warn!(brain = %queued_brain, %error, "could not resume queued Brain runs");
            }
        });
        let mut response = results.get();
        response.set_runtime_revision(runtime_revision);
        response.set_checkpoint_json(&checkpoint_json);
        Promise::ok(())
    }

    fn brain_service(
        &mut self,
        _params: finch_daemon::BrainServiceParams,
        mut results: finch_daemon::BrainServiceResults,
    ) -> Promise<(), capnp::Error> {
        let service: brain_service::Client = capnp_rpc::new_client(BrainServiceImpl {
            server: Arc::clone(&self.server),
        });
        results.get().set_service(service);
        Promise::ok(())
    }

    // ---- health ----------------------------------------------------------

    fn ping(
        &mut self,
        _params: finch_daemon::PingParams,
        mut results: finch_daemon::PingResults,
    ) -> Promise<(), capnp::Error> {
        results.get().set_version(env!("CARGO_PKG_VERSION"));
        Promise::ok(())
    }
}

fn program_language_to_capnp(
    language: crate::brain::shared::ProgramLanguage,
) -> finch_ipc_capnp::ProgramLanguage {
    match language {
        crate::brain::shared::ProgramLanguage::Forth => finch_ipc_capnp::ProgramLanguage::Forth,
        crate::brain::shared::ProgramLanguage::Lisp => finch_ipc_capnp::ProgramLanguage::Lisp,
    }
}

fn attachment_role_from_capnp(
    role: finch_ipc_capnp::BrainAttachmentRole,
) -> crate::brain::shared::AttachmentRole {
    match role {
        finch_ipc_capnp::BrainAttachmentRole::Runner => {
            crate::brain::shared::AttachmentRole::Runner
        }
        finch_ipc_capnp::BrainAttachmentRole::Driver => {
            crate::brain::shared::AttachmentRole::Driver
        }
        finch_ipc_capnp::BrainAttachmentRole::Consultant => {
            crate::brain::shared::AttachmentRole::Consultant
        }
        finch_ipc_capnp::BrainAttachmentRole::Observer => {
            crate::brain::shared::AttachmentRole::Observer
        }
    }
}

fn program_language_from_capnp(
    language: finch_ipc_capnp::ProgramLanguage,
) -> crate::brain::shared::ProgramLanguage {
    match language {
        finch_ipc_capnp::ProgramLanguage::Forth => crate::brain::shared::ProgramLanguage::Forth,
        finch_ipc_capnp::ProgramLanguage::Lisp => crate::brain::shared::ProgramLanguage::Lisp,
    }
}

fn decode_runner_program_result(
    result: capnp::Result<finch_ipc_capnp::brain_program_result::Reader<'_>>,
) -> Result<crate::server::RunnerProgramResult, String> {
    let result = result.map_err(|error| error.to_string())?;
    let error = result
        .get_error()
        .ok()
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !error.is_empty() {
        return Err(error.to_string());
    }
    let checkpoint = serde_json::from_slice(
        result
            .get_checkpoint_json()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(crate::server::RunnerProgramResult {
        output: result
            .get_output()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
    })
}

fn decode_runner_turn_result(
    result: capnp::Result<finch_ipc_capnp::brain_turn_result::Reader<'_>>,
) -> Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError> {
    let result = result.map_err(|error| error.to_string())?;
    let error = result
        .get_error()
        .ok()
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut turn_events = Vec::new();
    let encoded_turn_events = result
        .get_turn_events()
        .map_err(|error| error.to_string())?;
    for encoded in encoded_turn_events.iter() {
        turn_events.push(decode_runner_turn_event(encoded)?);
    }
    if !error.is_empty() {
        return Err(crate::server::RunnerTurnError {
            message: error.to_string(),
            turn_events,
        });
    }
    let checkpoint = serde_json::from_slice(
        result
            .get_checkpoint_json()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(crate::server::RunnerTurnResult {
        source: result
            .get_source()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        language: program_language_from_capnp(
            result.get_language().map_err(|error| error.to_string())?,
        ),
        output: result
            .get_output()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        turn_events,
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
    })
}

fn decode_runner_turn_event(
    encoded: finch_ipc_capnp::brain_turn_event::Reader<'_>,
) -> Result<crate::server::RunnerTurnEvent, String> {
    let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
        value
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let tool_id = text(encoded.get_tool_id());
    match encoded.get_kind().map_err(|error| error.to_string())? {
        finch_ipc_capnp::BrainTurnEventKind::Call => Ok(crate::server::RunnerTurnEvent::Call {
            tool_id,
            name: text(encoded.get_name()),
            input: serde_json::from_slice(
                encoded
                    .get_input_json()
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        }),
        finch_ipc_capnp::BrainTurnEventKind::Result => Ok(crate::server::RunnerTurnEvent::Result {
            tool_id,
            output: text(encoded.get_output()),
            is_error: encoded.get_is_error(),
        }),
        finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested => {
            Ok(crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: text(encoded.get_approval_id()),
                approval_kind: text(encoded.get_approval_kind()),
                subject: text(encoded.get_subject()),
                audience: decode_approval_audience(
                    encoded
                        .get_approval_audience()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
                detail: serde_json::from_slice(
                    encoded
                        .get_detail_json()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            })
        }
        finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided => {
            Ok(crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id: text(encoded.get_approval_id()),
                decision: serde_json::from_slice(
                    encoded
                        .get_decision_json()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_runner_turn_result, execute_typed_forth_ipc};
    use crate::ipc::brain_codec::encode_approval_audience;

    fn test_approval_audience() -> crate::brain::shared::BrainApprovalAudience {
        crate::brain::shared::BrainApprovalAudience {
            brain_id: crate::brain::shared::BrainId(
                uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            ),
            brain: "shared".into(),
            attachment_id: crate::brain::shared::AttachmentId(
                uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
            ),
            subject: "alice@box.local".into(),
            role: crate::brain::shared::AttachmentRole::Driver,
            environment_generation: 3,
        }
    }

    #[test]
    fn runner_turn_result_decodes_ordered_capnp_lifecycle() {
        let runtime = crate::runtime::ProgramRuntime::new();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        let checkpoint_json = serde_json::to_vec(&checkpoint).unwrap();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
            result.set_source("(say \"done\")");
            result.set_language(super::finch_ipc_capnp::ProgramLanguage::Lisp);
            result.set_output("done");
            result.set_runtime_revision(1);
            result.set_checkpoint_json(&checkpoint_json);
            result.set_error("");
            let mut events = result.init_turn_events(4);
            let mut call = events.reborrow().get(0);
            call.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Call);
            call.set_tool_id("tool-1");
            call.set_name("search_word");
            call.set_input_json(br#"{"query":"fib"}"#);
            let mut approval = events.reborrow().get(1);
            approval.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested);
            approval.set_approval_id("tool-1");
            approval.set_approval_kind("tool");
            approval.set_subject("search_word");
            encode_approval_audience(
                approval.reborrow().init_approval_audience(),
                &test_approval_audience(),
            );
            approval.set_detail_json(br#"{"input":{"query":"fib"}}"#);
            let mut decision = events.reborrow().get(2);
            decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            decision.set_approval_id("tool-1");
            decision.set_decision_json(br#"{"choice":"approve_once"}"#);
            let mut tool_result = events.reborrow().get(3);
            tool_result.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Result);
            tool_result.set_tool_id("tool-1");
            tool_result.set_output("found");
            tool_result.set_is_error(false);
        }

        let reader = message
            .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
            .unwrap();
        let decoded = decode_runner_turn_result(Ok(reader)).unwrap();
        assert_eq!(
            decoded.turn_events,
            vec![
                crate::server::RunnerTurnEvent::Call {
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
                crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: test_approval_audience(),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
                crate::server::RunnerTurnEvent::ApprovalDecided {
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
                crate::server::RunnerTurnEvent::Result {
                    tool_id: "tool-1".into(),
                    output: "found".into(),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn runner_turn_error_keeps_partial_lifecycle() {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
            result.set_error("provider failed after approval");
            let mut events = result.init_turn_events(1);
            let mut decision = events.reborrow().get(0);
            decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            decision.set_approval_id("approval-1");
            decision.set_decision_json(br#"{"choice":"deny"}"#);
        }

        let reader = message
            .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
            .unwrap();
        let error = decode_runner_turn_result(Ok(reader)).unwrap_err();
        assert_eq!(error.message, "provider failed after approval");
        assert_eq!(
            error.turn_events,
            vec![crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id: "approval-1".into(),
                decision: serde_json::json!({"choice": "deny"}),
            }]
        );
    }

    #[tokio::test]
    async fn eval_forth_uses_typed_signatures_and_runtime() {
        let (stack, output) = execute_typed_forth_ipc(
            ": double ( S n:int -- S int ! pure ) n n + ; 21 double".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(stack, vec![42]);
        assert!(output.is_empty());

        let error = execute_typed_forth_ipc(": legacy dup * ; 4 legacy".to_string())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("E-FORTH-SIG-001"));
    }
}

// ---------------------------------------------------------------------------
// Enum conversion helper
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Accept loop — call this from daemon startup
// ---------------------------------------------------------------------------

/// Bind the Unix socket and accept Cap'n Proto connections in a `LocalSet`.
///
/// This function never returns under normal operation.
pub async fn start_ipc_server(server: Arc<AgentServer>) -> Result<()> {
    let path = crate::ipc::transport::sock_path();

    // Remove stale socket file if present (crash recovery).
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)?;
    tracing::info!(path = %path.display(), "IPC server listening");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        let server = Arc::clone(&server);
                        tokio::task::spawn_local(async move {
                            if let Err(e) = handle_connection(stream, server).await {
                                tracing::warn!("IPC connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("IPC accept error: {}", e);
                    }
                }
            }
        })
        .await;
    Ok(())
}

async fn handle_connection(stream: tokio::net::UnixStream, server: Arc<AgentServer>) -> Result<()> {
    let (reader, writer) = stream.into_split();

    let network = twoparty::VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    );

    let daemon_impl = FinchDaemonImpl::new(server);
    let daemon_client: finch_daemon::Client = capnp_rpc::new_client(daemon_impl);

    RpcSystem::new(Box::new(network), Some(daemon_client.client))
        .await
        .map_err(anyhow::Error::from)
}
