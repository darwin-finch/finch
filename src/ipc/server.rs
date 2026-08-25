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

use crate::ipc::schema::finch_ipc_capnp::{self, finch_daemon};
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
            other => anyhow::bail!(
                "evalForth IPC supports only integer stack results; found {other:?}"
            ),
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
        let registration_id = broker.register(brain.clone(), lease_id, tx);
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
                                decode_runner_program_result(reply.get().and_then(|r| r.get_result())),
                                false,
                            ),
                            Err(error) => (Err(error.to_string()), true),
                        };
                        let _ = request.response_tx.send(result);
                        disconnected
                    }
                    crate::server::RunnerRequest::Turn(request) => {
                        let context_json = serde_json::to_vec(&request.context)
                            .map_err(|error| error.to_string());
                        let (result, disconnected) = match context_json {
                            Ok(context_json) => {
                                let mut call = runner.run_turn_request();
                                {
                                    let mut payload = call.get().init_request();
                                    payload.set_brain(&request.brain);
                                    payload.set_request_seq(request.request_seq);
                                    payload.set_prompt(&request.prompt);
                                    payload.set_context_json(&context_json);
                                }
                                match call.send().promise.await {
                                    Ok(reply) => (
                                        decode_runner_turn_result(
                                            reply.get().and_then(|r| r.get_result()),
                                        ),
                                        false,
                                    ),
                                    Err(error) => (Err(error.to_string()), true),
                                }
                            }
                            Err(error) => (Err(error), false),
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
        let mut response = results.get();
        response.set_runtime_revision(runtime_revision);
        response.set_checkpoint_json(&checkpoint_json);
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
        result.get_checkpoint_json().map_err(|error| error.to_string())?,
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
) -> Result<crate::server::RunnerTurnResult, String> {
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
        result.get_checkpoint_json().map_err(|error| error.to_string())?,
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
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::execute_typed_forth_ipc;

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
