//! ChatGPT subscription access through the supported Codex app-server boundary.
//!
//! Finch never reads or stores ChatGPT OAuth tokens. Codex owns managed login,
//! refresh, revocation, audience checks, and credential persistence. Each
//! provider request uses an ephemeral thread so Finch/Brain remains the sole
//! durable conversation authority.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use super::types::{ProviderRequest, ProviderResponse, StreamChunk};
use super::LlmProvider;
use crate::claude::types::ContentBlock;
use crate::tools::types::ToolDefinition;

pub const MANAGED_CODEX_CREDENTIAL_REF: &str = "codex-app-server:managed";
const MAX_RPC_LINE_BYTES: usize = 2 * 1024 * 1024;
const ADAPTER_INSTRUCTIONS: &str = "You are serving as Finch's model adapter. Do not modify files, run commands, browse, or invoke built-in Codex tools. Answer only from the supplied conversation. When Finch dynamic tools are supplied, invoke only those dynamic tools. Finch/Brain is the durable conversation authority; this Codex thread is ephemeral.";

#[derive(Debug, Clone)]
struct AppServerCommand {
    program: PathBuf,
    args: Vec<String>,
}

impl AppServerCommand {
    fn production() -> Self {
        Self {
            program: PathBuf::from("codex"),
            args: vec!["app-server".to_string()],
        }
    }

    #[cfg(test)]
    fn test(program: PathBuf, args: Vec<String>) -> Self {
        Self { program, args }
    }

    fn detect_dynamic_tools(&self) -> bool {
        if self.program.file_name().and_then(|name| name.to_str()) != Some("codex") {
            return false;
        }
        let Ok(directory) = tempfile::tempdir() else {
            return false;
        };
        let status = std::process::Command::new(&self.program)
            .args(["app-server", "generate-json-schema", "--out"])
            .arg(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !matches!(status, Ok(status) if status.success()) {
            return false;
        }
        let schema = directory.path().join("v2").join("ThreadStartParams.json");
        std::fs::read(&schema)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.pointer("/properties/dynamicTools").cloned())
            .is_some()
    }
}

struct RpcClient {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    queued: VecDeque<Value>,
    next_id: u64,
}

impl RpcClient {
    async fn spawn(command: &AppServerCommand) -> Result<Self> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                "Could not start Codex app-server; install or update the Codex CLI".to_string()
            })?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server stdin was unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server stdout was unavailable")?;
        let mut client = Self {
            _child: child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            queued: VecDeque::new(),
            next_id: 1,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "finch",
                    "title": "Finch",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": true }
            }),
        )
        .await?;
        self.send(json!({ "method": "initialized", "params": {} }))
            .await
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value).context("Failed to encode Codex RPC request")?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .context("Codex app-server transport closed")?;
        self.stdin
            .flush()
            .await
            .context("Codex app-server transport closed")
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(json!({ "method": method, "id": id, "params": params }))
            .await?;
        loop {
            let message = self.next_message().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                    bail!("Codex app-server rejected {method} (RPC {code})");
                }
                return message
                    .get("result")
                    .cloned()
                    .context("Codex app-server returned an invalid response");
            }
            self.queued.push_back(message);
        }
    }

    async fn next_event(&mut self) -> Result<Value> {
        if let Some(message) = self.queued.pop_front() {
            return Ok(message);
        }
        self.next_message().await
    }

    async fn next_message(&mut self) -> Result<Value> {
        let line = self
            .lines
            .next_line()
            .await
            .context("Failed to read Codex app-server response")?
            .context("Codex app-server exited unexpectedly")?;
        if line.len() > MAX_RPC_LINE_BYTES {
            bail!("Codex app-server response exceeded the size limit");
        }
        serde_json::from_str(&line).context("Codex app-server returned invalid JSON")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAccountStatus {
    pub signed_in: bool,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChatGptLogin {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
}

pub struct ChatGptDeviceLogin {
    pub details: PendingChatGptLogin,
    client: RpcClient,
}

#[derive(Clone)]
pub struct CodexAppServerAuth {
    command: AppServerCommand,
}

impl CodexAppServerAuth {
    pub fn new() -> Self {
        Self {
            command: AppServerCommand::production(),
        }
    }

    #[cfg(test)]
    fn with_command(command: AppServerCommand) -> Self {
        Self { command }
    }

    pub async fn status(&self, refresh: bool) -> Result<ChatGptAccountStatus> {
        let mut client = RpcClient::spawn(&self.command).await?;
        let result = client
            .request("account/read", json!({ "refreshToken": refresh }))
            .await?;
        let account = result.get("account").filter(|account| !account.is_null());
        let signed_in = account
            .and_then(|account| account.get("type"))
            .and_then(Value::as_str)
            == Some("chatgpt");
        Ok(ChatGptAccountStatus {
            signed_in,
            plan_type: signed_in
                .then(|| {
                    account
                        .and_then(|account| account.get("planType"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .flatten(),
        })
    }

    pub async fn begin_device_login(&self) -> Result<ChatGptDeviceLogin> {
        let mut client = RpcClient::spawn(&self.command).await?;
        let result = client
            .request(
                "account/login/start",
                json!({ "type": "chatgptDeviceCode" }),
            )
            .await?;
        let pending = PendingChatGptLogin {
            login_id: required_string(&result, "loginId")?,
            verification_url: required_string(&result, "verificationUrl")?,
            user_code: required_string(&result, "userCode")?,
        };
        Ok(ChatGptDeviceLogin {
            details: pending,
            client,
        })
    }

    pub async fn finish_device_login(
        &self,
        mut login: ChatGptDeviceLogin,
    ) -> Result<()> {
        loop {
            let event = login.client.next_event().await?;
            if event.get("method").and_then(Value::as_str) != Some("account/login/completed") {
                continue;
            }
            let params = event.get("params").context("Invalid login notification")?;
            if params.get("loginId").and_then(Value::as_str) != Some(&login.details.login_id) {
                continue;
            }
            if params.get("success").and_then(Value::as_bool) == Some(true) {
                return Ok(());
            }
            bail!("ChatGPT login did not complete successfully");
        }
    }

    pub async fn logout(&self) -> Result<()> {
        let mut client = RpcClient::spawn(&self.command).await?;
        client.request("account/logout", json!({})).await?;
        Ok(())
    }
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("Codex app-server omitted {field}"))
}

pub struct CodexAppServerProvider {
    command: AppServerCommand,
    credential_ref: String,
    default_model: String,
    dynamic_tools: bool,
}

impl CodexAppServerProvider {
    pub fn new(credential_ref: String, default_model: String) -> Result<Self> {
        if credential_ref != MANAGED_CODEX_CREDENTIAL_REF {
            bail!("Unsupported ChatGPT credential reference");
        }
        let command = AppServerCommand::production();
        let dynamic_tools = command.detect_dynamic_tools();
        Ok(Self {
            command,
            credential_ref,
            default_model,
            dynamic_tools,
        })
    }

    #[cfg(test)]
    fn with_command(
        command: AppServerCommand,
        default_model: impl Into<String>,
        dynamic_tools: bool,
    ) -> Self {
        Self {
            command,
            credential_ref: MANAGED_CODEX_CREDENTIAL_REF.to_string(),
            default_model: default_model.into(),
            dynamic_tools,
        }
    }

    async fn begin_turn(&self, request: &ProviderRequest) -> Result<TurnSession> {
        if self.credential_ref != MANAGED_CODEX_CREDENTIAL_REF {
            bail!("Unsupported ChatGPT credential reference");
        }
        if request.tools.as_ref().is_some_and(|tools| !tools.is_empty()) && !self.dynamic_tools {
            bail!("Installed Codex app-server does not advertise dynamic tool support");
        }
        let mut rpc = RpcClient::spawn(&self.command).await?;
        let account = rpc
            .request("account/read", json!({ "refreshToken": true }))
            .await?;
        let account_type = account
            .pointer("/account/type")
            .and_then(Value::as_str)
            .unwrap_or("signed_out");
        if account_type != "chatgpt" {
            bail!("ChatGPT subscription profile is not signed in; run `finch auth login chatgpt`");
        }
        let isolated_cwd = tempfile::tempdir()
            .context("Could not create an isolated Codex adapter workspace")?;

        let model = if request.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };
        let mut thread_params = json!({
            "model": model,
            "ephemeral": true,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "cwd": isolated_cwd.path(),
            "serviceName": "finch",
            "developerInstructions": adapter_instructions(request.system.as_deref())
        });
        if let Some(tools) = request.tools.as_ref().filter(|tools| !tools.is_empty()) {
            thread_params["dynamicTools"] = dynamic_tools(tools);
        }
        let thread = rpc.request("thread/start", thread_params).await?;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("Codex app-server omitted the ephemeral thread id")?
            .to_string();
        let turn = rpc
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": conversation_payload(request)? }],
                    "model": model
                }),
            )
            .await?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("Codex app-server omitted the turn id")?
            .to_string();
        Ok(TurnSession {
            rpc,
            thread_id,
            turn_id,
            _isolated_cwd: isolated_cwd,
        })
    }
}

fn adapter_instructions(system: Option<&str>) -> String {
    match system.filter(|system| !system.trim().is_empty()) {
        Some(system) => format!("{ADAPTER_INSTRUCTIONS}\n\nFinch system instructions:\n{system}"),
        None => ADAPTER_INSTRUCTIONS.to_string(),
    }
}

fn dynamic_tools(tools: &[ToolDefinition]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": {
                        "type": tool.input_schema.schema_type,
                        "properties": tool.input_schema.properties,
                        "required": tool.input_schema.required
                    }
                })
            })
            .collect(),
    )
}

fn conversation_payload(request: &ProviderRequest) -> Result<String> {
    let messages = serde_json::to_string(&request.messages)
        .context("Failed to encode Finch conversation for Codex app-server")?;
    Ok(format!(
        "The following JSON is the complete authoritative Finch conversation. Continue it as the assistant. Preserve tool_use/tool_result relationships exactly.\n<finch_conversation_json>{messages}</finch_conversation_json>"
    ))
}

struct TurnSession {
    rpc: RpcClient,
    thread_id: String,
    turn_id: String,
    _isolated_cwd: tempfile::TempDir,
}

impl TurnSession {
    async fn drive(mut self, tx: mpsc::Sender<Result<StreamChunk>>) {
        let mut text = String::new();
        loop {
            let event = match self.rpc.next_event().await {
                Ok(event) => event,
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };
            match event.get("method").and_then(Value::as_str) {
                Some("item/agentMessage/delta") => {
                    if let Some(delta) = event.pointer("/params/delta").and_then(Value::as_str) {
                        text.push_str(delta);
                        if tx
                            .send(Ok(StreamChunk::TextDelta(delta.to_string())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Some("item/tool/call") => {
                    let Some(params) = event.get("params") else {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("Invalid Codex dynamic-tool request")))
                            .await;
                        return;
                    };
                    let id = params
                        .get("callId")
                        .and_then(Value::as_str)
                        .unwrap_or("codex-tool-call")
                        .to_string();
                    let name = params
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let input = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let interrupt_id = self.rpc.next_id;
                    self.rpc.next_id = self.rpc.next_id.saturating_add(1);
                    let _ = self
                        .rpc
                        .send(json!({
                            "method": "turn/interrupt",
                            "id": interrupt_id,
                            "params": { "threadId": self.thread_id, "turnId": self.turn_id }
                        }))
                        .await;
                    if !text.is_empty() {
                        let _ = tx
                            .send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text {
                                text: text.clone(),
                            })))
                            .await;
                    }
                    let _ = tx
                        .send(Ok(StreamChunk::ContentBlockComplete(
                            ContentBlock::ToolUse { id, name, input },
                        )))
                        .await;
                    return;
                }
                Some("turn/completed") => {
                    let status = event
                        .pointer("/params/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("failed");
                    if status != "completed" {
                        let _ = tx
                            .send(Err(anyhow::anyhow!(
                                "Codex app-server turn ended with status {status}"
                            )))
                            .await;
                        return;
                    }
                    if text.is_empty() {
                        if let Some(final_text) = completed_agent_text(&event) {
                            text = final_text;
                            let _ = tx.send(Ok(StreamChunk::TextDelta(text.clone()))).await;
                        }
                    }
                    if !text.is_empty() {
                        let _ = tx
                            .send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text {
                                text,
                            })))
                            .await;
                    }
                    return;
                }
                _ => {}
            }
        }
    }
}

fn completed_agent_text(event: &Value) -> Option<String> {
    event
        .pointer("/params/turn/items")
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))?
        .get("text")?
        .as_str()
        .map(str::to_string)
}

#[async_trait]
impl LlmProvider for CodexAppServerProvider {
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let model = if request.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };
        let mut receiver = self.send_message_stream(request).await?;
        let mut content = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            if let StreamChunk::ContentBlockComplete(block) = chunk? {
                content.push(block);
            }
        }
        let stop_reason = content
            .iter()
            .any(ContentBlock::is_tool_use)
            .then_some("tool_use".to_string())
            .or_else(|| Some("end_turn".to_string()));
        Ok(ProviderResponse {
            id: format!("codex-ephemeral-{}", uuid::Uuid::new_v4()),
            model,
            content,
            stop_reason,
            role: "assistant".to_string(),
            provider: "chatgpt_subscription".to_string(),
        })
    }

    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let session = self.begin_turn(request).await?;
        let (tx, rx) = mpsc::channel(100);
        tokio::spawn(session.drive(tx));
        Ok(rx)
    }

    fn name(&self) -> &str {
        "chatgpt_subscription"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports_tools(&self) -> bool {
        self.dynamic_tools
    }

    fn context_limit_tokens(&self) -> usize {
        120_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::Message;

    fn mock_app_server(account_type: &str) -> (tempfile::TempDir, AppServerCommand) {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("mock_app_server.py");
        std::fs::write(
            &script,
            r#"import json, sys
account_type = sys.argv[1]
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    ident = m.get('id')
    if method == 'initialized':
        continue
    if method == 'initialize':
        result = {'capabilities': {}}
    elif method == 'account/read':
        assert m['params']['refreshToken'] is True
        result = {'account': {'type': account_type, 'planType': 'plus'}}
    elif method == 'account/login/start':
        assert m['params']['type'] == 'chatgptDeviceCode'
        result = {'type': 'chatgptDeviceCode', 'loginId': 'login-1', 'verificationUrl': 'https://example.invalid/device', 'userCode': 'ABCD'}
    elif method == 'account/logout':
        result = {}
    elif method == 'thread/start':
        p = m['params']
        assert p['ephemeral'] is True
        assert p['approvalPolicy'] == 'never'
        assert p['sandbox'] == 'read-only'
        assert 'finch-chatgpt-provider' not in p['cwd']
        assert 'dynamicTools' not in p
        result = {'thread': {'id': 'ephemeral-thread'}}
    elif method == 'turn/start':
        assert 'complete authoritative Finch conversation' in m['params']['input'][0]['text']
        result = {'turn': {'id': 'turn-1'}}
    else:
        result = {}
    print(json.dumps({'id': ident, 'result': result}), flush=True)
    if method == 'account/login/start':
        print(json.dumps({'method': 'account/login/completed', 'params': {'loginId': 'login-1', 'success': True}}), flush=True)
    if method == 'turn/start':
        print(json.dumps({'method': 'item/agentMessage/delta', 'params': {'delta': 'hello'}}), flush=True)
        print(json.dumps({'method': 'turn/completed', 'params': {'turn': {'status': 'completed'}}}), flush=True)
"#,
        )
        .unwrap();
        let command = AppServerCommand::test(
            PathBuf::from("python3"),
            vec![script.to_string_lossy().into_owned(), account_type.to_string()],
        );
        (directory, command)
    }

    #[test]
    fn rejects_non_managed_credential_references() {
        let error = CodexAppServerProvider::new("raw-oauth-token".into(), "model".into())
            .err()
            .expect("invalid reference must fail");
        assert_eq!(error.to_string(), "Unsupported ChatGPT credential reference");
    }

    #[tokio::test]
    async fn tool_request_fails_before_spawning_when_capability_is_absent() {
        let provider = CodexAppServerProvider::with_command(
            AppServerCommand::test(PathBuf::from("/does/not/exist"), vec![]),
            "model",
            false,
        );
        let request = ProviderRequest::new(vec![]).with_tools(vec![ToolDefinition {
            name: "lookup".into(),
            description: "lookup".into(),
            input_schema: crate::tools::types::ToolInputSchema::simple(vec![]),
        }]);
        let error = provider.send_message_stream(&request).await.unwrap_err();
        assert!(error.to_string().contains("does not advertise dynamic tool support"));
        assert!(!error.to_string().contains("does/not/exist"));
    }

    #[test]
    fn authoritative_payload_contains_complete_tool_history() {
        let request = ProviderRequest::new(vec![Message {
            role: "user".into(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "result".into(),
                is_error: None,
            }],
        }]);
        let payload = conversation_payload(&request).unwrap();
        assert!(payload.contains("call-1"));
        assert!(payload.contains("complete authoritative Finch conversation"));
    }

    #[tokio::test]
    async fn mock_app_server_conforms_to_ephemeral_read_only_text_contract() {
        let (_directory, command) = mock_app_server("chatgpt");
        let provider = CodexAppServerProvider::with_command(command, "test-model", false);
        let request = ProviderRequest::new(vec![Message {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: "hi".into() }],
        }]);
        let response = provider.send_message(&request).await.unwrap();
        assert_eq!(response.content[0].as_text(), Some("hello"));
        assert_eq!(response.provider, "chatgpt_subscription");
    }

    #[tokio::test]
    async fn managed_auth_status_device_login_and_logout_use_app_server() {
        let (_directory, command) = mock_app_server("chatgpt");
        let auth = CodexAppServerAuth::with_command(command);
        let status = auth.status(true).await.unwrap();
        assert_eq!(status.plan_type.as_deref(), Some("plus"));
        let login = auth.begin_device_login().await.unwrap();
        assert_eq!(login.details.user_code, "ABCD");
        auth.finish_device_login(login).await.unwrap();
        auth.logout().await.unwrap();
    }

    #[tokio::test]
    async fn api_key_account_is_not_accepted_as_subscription_auth() {
        let (_directory, command) = mock_app_server("apiKey");
        let provider = CodexAppServerProvider::with_command(command, "test-model", false);
        let error = provider
            .send_message(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"));
        assert!(!error.to_string().contains("apiKey"));
    }

    #[tokio::test]
    #[ignore = "requires FINCH_LIVE_CHATGPT_APP_SERVER=1 and an existing managed Codex login"]
    async fn live_managed_subscription_smoke_test() {
        if std::env::var("FINCH_LIVE_CHATGPT_APP_SERVER").as_deref() != Ok("1") {
            return;
        }
        let provider = CodexAppServerProvider::new(
            MANAGED_CODEX_CREDENTIAL_REF.into(),
            "gpt-5.6-terra".into(),
        )
        .unwrap();
        let request = ProviderRequest::new(vec![Message {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: "Reply with ok".into() }],
        }]);
        let response = provider.send_message(&request).await.unwrap();
        assert!(!response.content.is_empty());
    }
}
