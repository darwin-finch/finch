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
use uuid::Uuid;

use crate::ipc::schema::finch_ipc_capnp::{
    self, BrainState as CapnpBrainState, finch_daemon,
};
use crate::server::{AgentServer, PlanResponse};

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
            serde_json::from_str(td.get_input_schema_json()?.to_str()?)
                .unwrap_or_else(|_| crate::tools::types::ToolInputSchema {
                    schema_type: "object".to_string(),
                    properties: serde_json::Value::Object(serde_json::Map::new()),
                    required: vec![],
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

    // ---- brain management ------------------------------------------------

    fn spawn_brain(
        &mut self,
        params: finch_daemon::SpawnBrainParams,
        mut results: finch_daemon::SpawnBrainResults,
    ) -> Promise<(), capnp::Error> {
        use crate::brain::daemon_brain::run_daemon_brain_loop;

        let p = pry!(params.get());
        let task = pry!(p.get_task_description()).to_str().unwrap_or("").to_string();
        let provider_name = pry!(p.get_provider()).to_str().unwrap_or("").to_string();
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let id = Uuid::new_v4();
            let registry = Arc::clone(server.brain_registry());

            let provider = server
                .provider_for_name(if provider_name.is_empty() { None } else { Some(&provider_name) })
                .cloned()
                .ok_or_else(|| capnp::Error::failed("No provider configured for daemon brains".into()))?;

            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "~".to_string());

            registry.insert(id, task.clone()).await;

            let registry_clone = Arc::clone(&registry);
            let task_clone = task.clone();
            let cwd_clone = cwd.clone();
            tokio::spawn(async move {
                run_daemon_brain_loop(id, task_clone, registry_clone, provider, cwd_clone).await;
            });

            results.get().set_id(id.to_string().as_str());
            Ok(())
        })
    }

    fn list_brains(
        &mut self,
        _params: finch_daemon::ListBrainsParams,
        mut results: finch_daemon::ListBrainsResults,
    ) -> Promise<(), capnp::Error> {
        let server = Arc::clone(&self.server);
        Promise::from_future(async move {
            let summaries = server.brain_registry().list_all().await;
            let mut list = results.get().init_brains(summaries.len() as u32);
            for (i, s) in summaries.iter().enumerate() {
                let mut b = list.reborrow().get(i as u32);
                b.set_id(s.id.to_string().as_str());
                b.set_name(s.name.as_str());
                b.set_task(s.task.as_str());
                b.set_state(brain_state_from_server(&s.state));
                b.set_age_secs(s.age_secs);
            }
            Ok(())
        })
    }

    fn get_brain(
        &mut self,
        params: finch_daemon::GetBrainParams,
        mut results: finch_daemon::GetBrainResults,
    ) -> Promise<(), capnp::Error> {
        let id_str = pry!(pry!(params.get()).get_id()).to_str().unwrap_or("").to_string();
        let id = pry!(Uuid::parse_str(&id_str)
            .map_err(|e| capnp::Error::failed(e.to_string())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let detail = server
                .brain_registry()
                .get_detail(id)
                .await
                .ok_or_else(|| capnp::Error::failed(format!("brain {} not found", id)))?;
            let mut d = results.get().init_details();
            d.set_id(detail.id.to_string().as_str());
            d.set_name(detail.name.as_str());
            d.set_task(detail.task.as_str());
            d.set_state(brain_state_from_server(&detail.state));
            if let Some(pq) = &detail.pending_question {
                d.set_question(pq.question.as_str());
                let mut ol = d.reborrow().init_question_options(pq.options.len() as u32);
                for (i, o) in pq.options.iter().enumerate() {
                    ol.set(i as u32, o.as_str());
                }
            }
            if let Some(pp) = &detail.pending_plan {
                d.set_plan(pp.plan.as_str());
            }
            if let Some(summary) = &detail.final_summary {
                d.set_result(summary.as_str());
            }
            let logs = &detail.event_log;
            let mut el = d.init_event_log(logs.len() as u32);
            for (i, line) in logs.iter().enumerate() {
                el.set(i as u32, line.as_str());
            }
            Ok(())
        })
    }

    fn answer_brain_question(
        &mut self,
        params: finch_daemon::AnswerBrainQuestionParams,
        _results: finch_daemon::AnswerBrainQuestionResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let id_str = pry!(p.get_id()).to_str().unwrap_or("").to_string();
        let answer = pry!(p.get_answer()).to_str().unwrap_or("").to_string();
        let id = pry!(Uuid::parse_str(&id_str)
            .map_err(|e| capnp::Error::failed(e.to_string())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            server
                .brain_registry()
                .answer_question(id, answer)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))
        })
    }

    fn respond_to_brain_plan(
        &mut self,
        params: finch_daemon::RespondToBrainPlanParams,
        _results: finch_daemon::RespondToBrainPlanResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let id_str = pry!(p.get_id()).to_str().unwrap_or("").to_string();
        let approved = p.get_approved();
        let instruction = pry!(p.get_instruction()).to_str().unwrap_or("").to_string();
        let id = pry!(Uuid::parse_str(&id_str)
            .map_err(|e| capnp::Error::failed(e.to_string())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let response = if approved {
                if instruction.is_empty() {
                    PlanResponse::Approve
                } else {
                    PlanResponse::ChangesRequested { feedback: instruction }
                }
            } else {
                PlanResponse::Reject
            };
            server
                .brain_registry()
                .respond_to_plan(id, response)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))
        })
    }

    fn cancel_brain(
        &mut self,
        params: finch_daemon::CancelBrainParams,
        _results: finch_daemon::CancelBrainResults,
    ) -> Promise<(), capnp::Error> {
        let id_str = pry!(pry!(params.get()).get_id()).to_str().unwrap_or("").to_string();
        let id = pry!(Uuid::parse_str(&id_str)
            .map_err(|e| capnp::Error::failed(e.to_string())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            server.brain_registry().cancel(id).await;
            Ok(())
        })
    }

    // ---- health ----------------------------------------------------------

    fn ping(
        &mut self,
        _params: finch_daemon::PingParams,
        mut results: finch_daemon::PingResults,
    ) -> Promise<(), capnp::Error> {
        results
            .get()
            .set_version(env!("CARGO_PKG_VERSION"));
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Enum conversion helper
// ---------------------------------------------------------------------------

fn brain_state_from_server(s: &crate::server::BrainState) -> CapnpBrainState {
    match s {
        crate::server::BrainState::Running => CapnpBrainState::Running,
        crate::server::BrainState::WaitingForInput => CapnpBrainState::WaitingForInput,
        crate::server::BrainState::PlanReady => CapnpBrainState::PlanReady,
        crate::server::BrainState::Dead => CapnpBrainState::Completed,
    }
}

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

async fn handle_connection(
    stream: tokio::net::UnixStream,
    server: Arc<AgentServer>,
) -> Result<()> {
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
