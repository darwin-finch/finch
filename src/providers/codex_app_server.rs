//! ChatGPT subscription access through the documented Codex app-server protocol.
//!
//! This module never reads ChatGPT credentials. Codex owns login, persistence,
//! refresh, and upstream transport. Finch launches an explicitly configured
//! absolute executable and speaks the stable stdio JSONL protocol.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Duration, Instant};

use super::{LlmProvider, ProviderRequest, ProviderResponse, StreamChunk};

/// The model Finch exposes through the subscription provider.
pub const GPT_5_6_SOL: &str = "gpt-5.6-sol";

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_MESSAGES: usize = 8_192;
const DEFAULT_MAX_QUEUED: usize = 256;
const DEFAULT_MAX_STDERR_BYTES: usize = 64 * 1024;
const MODEL_PAGE_LIMIT: u64 = 100;
const MAX_MODEL_PAGES: usize = 100;
const MAX_MODELS: usize = 10_000;

/// Process and protocol limits for one app-server connection.
#[derive(Clone, Debug)]
pub struct AppServerConfig {
    executable: Arc<PinnedExecutable>,
    rpc_timeout: Duration,
    operation_timeout: Duration,
    shutdown_timeout: Duration,
    max_frame_bytes: usize,
    max_total_bytes: usize,
    max_messages: usize,
    max_queued: usize,
    max_stderr_bytes: usize,
    #[cfg(test)]
    prefix_args: Vec<String>,
}

impl AppServerConfig {
    /// Pin an explicitly configured self-contained native Codex executable.
    ///
    /// Launcher symlinks (commonly installed by npm or Homebrew) are rejected:
    /// configure the absolute path to the native Codex binary they ultimately
    /// invoke. Finch copies bytes from a no-follow descriptor into a private
    /// staging directory so later source/ancestor replacement cannot change
    /// the program that is spawned.
    pub fn new(executable: impl AsRef<Path>) -> Result<Self> {
        let executable = Arc::new(PinnedExecutable::new(executable.as_ref())?);
        Ok(Self {
            executable,
            rpc_timeout: DEFAULT_RPC_TIMEOUT,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_messages: DEFAULT_MAX_MESSAGES,
            max_queued: DEFAULT_MAX_QUEUED,
            max_stderr_bytes: DEFAULT_MAX_STDERR_BYTES,
            #[cfg(test)]
            prefix_args: Vec::new(),
        })
    }

    /// The canonical executable path used for every spawn.
    pub fn executable(&self) -> &Path {
        &self.executable.path
    }

    #[cfg(test)]
    fn with_test_limits(mut self, frame: usize, total: usize, timeout: Duration) -> Self {
        self.max_frame_bytes = frame;
        self.max_total_bytes = total;
        self.rpc_timeout = timeout;
        self.operation_timeout = timeout;
        self.shutdown_timeout = timeout;
        self
    }

    #[cfg(test)]
    fn with_prefix_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.prefix_args = args.into_iter().collect();
        self
    }
}

struct PinnedExecutable {
    path: PathBuf,
    source: PathBuf,
    _staging: tempfile::TempDir,
}

impl fmt::Debug for PinnedExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedExecutable")
            .field("source", &self.source)
            .field("staged", &self.path)
            .finish_non_exhaustive()
    }
}

impl PinnedExecutable {
    fn new(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!("Codex app-server executable path must be absolute");
        }
        let path_metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("Could not inspect Codex executable at {}", path.display()))?;
        if path_metadata.file_type().is_symlink() {
            bail!(
            "Codex executable must be the self-contained native binary, not an npm/Homebrew launcher symlink; resolve the launcher and configure its native executable explicitly"
        );
        }
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("Could not open Codex executable at {}", path.display()))?;
        let metadata = source
            .metadata()
            .context("Could not inspect opened Codex executable")?;
        if !metadata.is_file() {
            bail!("Codex app-server executable is not a regular file");
        }
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("Codex app-server executable is not executable");
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            bail!("Codex app-server executable is group or world writable");
        }
        let mut magic = [0_u8; 4];
        source
            .read_exact(&mut magic)
            .context("Codex executable is too short to be a native binary")?;
        if !is_native_executable_magic(magic) {
            bail!(
                "Codex executable must be the self-contained native ELF or Mach-O binary, not an npm/Homebrew launcher script; resolve the launcher and configure its native executable explicitly"
            );
        }
        source
            .seek(SeekFrom::Start(0))
            .context("Could not rewind opened Codex executable")?;

        let staging = tempfile::Builder::new()
            .prefix("finch-codex-executable-")
            .tempdir()
            .context("Could not create private Codex executable staging directory")?;
        std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))?;
        let staged_path = staging.path().join("codex-native");
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&staged_path)
            .context("Could not create pinned Codex executable")?;
        std::io::copy(&mut source, &mut staged).context("Could not pin Codex executable bytes")?;
        staged
            .sync_all()
            .context("Could not sync pinned Codex executable")?;
        staged
            .set_permissions(std::fs::Permissions::from_mode(0o500))
            .context("Could not make pinned Codex executable immutable")?;
        let source = path.to_path_buf();
        Ok(Self {
            path: staged_path,
            source,
            _staging: staging,
        })
    }
}

fn is_native_executable_magic(magic: [u8; 4]) -> bool {
    magic == *b"\x7fELF"
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
}

fn inherited_environment() -> Result<Vec<(&'static str, std::ffi::OsString)>> {
    let mut values = ["HOME", "CODEX_HOME", "TMPDIR", "USER", "LOGNAME"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
        .collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    {
        values.extend(validated_linux_keyring_environment(
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("DBUS_SESSION_BUS_ADDRESS"),
        )?);
    }
    Ok(values)
}

#[cfg(target_os = "linux")]
fn validated_linux_keyring_environment(
    runtime: Option<std::ffi::OsString>,
    bus: Option<std::ffi::OsString>,
) -> Result<Vec<(&'static str, std::ffi::OsString)>> {
    use std::os::unix::fs::MetadataExt;
    let (runtime, bus) = match (runtime, bus) {
        (None, None) => return Ok(Vec::new()),
        (Some(runtime), Some(bus)) => (runtime, bus),
        _ => bail!("Linux keyring environment is incomplete; XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS must be supplied together"),
    };
    let runtime_path = Path::new(&runtime);
    let expected = format!("unix:path={}/bus", runtime_path.display());
    if !runtime_path.is_absolute() || bus.to_string_lossy() != expected {
        bail!("Linux keyring environment is unsafe; XDG_RUNTIME_DIR must be absolute and DBUS_SESSION_BUS_ADDRESS must name its bus socket exactly");
    }
    let metadata = std::fs::metadata(runtime_path).context(
        "Linux keyring environment is unavailable because XDG_RUNTIME_DIR cannot be inspected",
    )?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { nix::libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
    {
        bail!("Linux keyring environment is unsafe; XDG_RUNTIME_DIR must be an owner-only directory owned by the current user");
    }
    Ok(vec![
        ("XDG_RUNTIME_DIR", runtime),
        ("DBUS_SESSION_BUS_ADDRESS", bus),
    ])
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if nix::libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        unsafe {
            nix::libc::kill(-pid, nix::libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

#[derive(Debug, Default)]
struct StderrCapture {
    observed: usize,
    truncated: bool,
}

impl StderrCapture {
    fn push(&mut self, bytes: &[u8], limit: usize) {
        let remaining = limit.saturating_sub(self.observed);
        self.observed = self.observed.saturating_add(bytes.len().min(remaining));
        self.truncated |= bytes.len() > remaining;
    }

    fn redacted(&self) -> String {
        if self.observed == 0 {
            return String::new();
        }
        let mut lines = format!("[app-server stderr withheld: {} bytes]", self.observed);
        if self.truncated {
            lines.push_str("\n[app-server stderr truncated]");
        }
        lines
    }
}

async fn capture_stderr(
    mut stderr: tokio::process::ChildStderr,
    capture: Arc<Mutex<StderrCapture>>,
    limit: usize,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut guard = capture.lock().expect("stderr capture lock poisoned");
                guard.push(&buffer[..read], limit);
            }
        }
    }
}

#[derive(Debug)]
enum IncomingMessage {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    #[serde(default, rename = "message")]
    _message: String,
}

fn decode_message(value: Value) -> Result<IncomingMessage> {
    let object = value
        .as_object()
        .context("Codex app-server message was not a JSON object")?;
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);
    match (id, method) {
        (Some(id), Some(method)) => Ok(IncomingMessage::ServerRequest {
            id,
            method: method.to_string(),
            params: object.get("params").cloned().unwrap_or_else(|| json!({})),
        }),
        (None, Some(method)) => Ok(IncomingMessage::Notification {
            method: method.to_string(),
            params: object.get("params").cloned().unwrap_or_else(|| json!({})),
        }),
        (Some(id), None) => {
            let result = object.get("result").cloned();
            let error = object
                .get("error")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .context("Codex app-server returned an invalid RPC error")?;
            if result.is_some() == error.is_some() {
                bail!("Codex app-server response must contain exactly one of result or error");
            }
            Ok(IncomingMessage::Response { id, result, error })
        }
        (None, None) => bail!("Codex app-server message omitted id and method"),
    }
}

struct JsonlTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<StderrCapture>>,
    stderr_task: JoinHandle<()>,
    queued: VecDeque<(String, Value)>,
    next_id: u64,
    config: AppServerConfig,
    received_bytes: usize,
    received_messages: usize,
    sent_bytes: usize,
    sent_messages: usize,
    terminal_logins: HashSet<String>,
}

impl JsonlTransport {
    async fn spawn(config: AppServerConfig) -> Result<Self> {
        let mut command = Command::new(config.executable());
        #[cfg(test)]
        command.args(&config.prefix_args);
        command.args(["app-server", "--listen", "stdio://"]);
        command.env_clear();
        command.envs(inherited_environment()?);
        configure_process_group(&mut command);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Could not start Codex app-server at {}",
                    config.executable().display()
                )
            })?;
        let stdin = child.stdin.take().context("Codex stdin was unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex stdout was unavailable")?;
        let child_stderr = child
            .stderr
            .take()
            .context("Codex stderr was unavailable")?;
        let stderr = Arc::new(Mutex::new(StderrCapture::default()));
        let stderr_task = tokio::spawn(capture_stderr(
            child_stderr,
            Arc::clone(&stderr),
            config.max_stderr_bytes,
        ));
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            stderr_task,
            queued: VecDeque::new(),
            next_id: 1,
            config,
            received_bytes: 0,
            received_messages: 0,
            sent_bytes: 0,
            sent_messages: 0,
            terminal_logins: HashSet::new(),
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request_with_notifications(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "finch",
                    "title": "Finch",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
            &[],
        )
        .await?;
        self.send_notification("initialized", json!({})).await
    }

    async fn send_value(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value).context("Failed to encode Codex RPC message")?;
        bytes.push(b'\n');
        if bytes.len() > self.config.max_frame_bytes {
            bail!("Codex app-server outbound message exceeded the size limit");
        }
        let next_bytes = self.sent_bytes.saturating_add(bytes.len());
        let next_messages = self.sent_messages.saturating_add(1);
        if next_bytes > self.config.max_total_bytes || next_messages > self.config.max_messages {
            bail!("Codex app-server outbound stream exceeded aggregate limits");
        }
        self.stdin
            .write_all(&bytes)
            .await
            .context("Codex app-server stdin closed")?;
        self.stdin
            .flush()
            .await
            .context("Codex app-server stdin closed")?;
        self.sent_bytes = next_bytes;
        self.sent_messages = next_messages;
        Ok(())
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_value(&json!({ "method": method, "params": params }))
            .await
    }

    async fn request_with_notifications(
        &mut self,
        method: &str,
        params: Value,
        allowed_notifications: &[&str],
    ) -> Result<Value> {
        timeout(
            self.config.rpc_timeout,
            self.request_inner(method, params, allowed_notifications),
        )
        .await
        .with_context(|| format!("Codex app-server timed out during {method}"))?
    }

    async fn request_inner(
        &mut self,
        method: &str,
        params: Value,
        allowed_notifications: &[&str],
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("Codex app-server request id overflowed")?;
        self.send_value(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        loop {
            match self.read_message().await? {
                IncomingMessage::Response {
                    id: response_id,
                    result,
                    error,
                } if response_id.as_u64() == Some(id) => {
                    if let Some(error) = error {
                        // Server error text can contain upstream or auth data.
                        // Preserve only the typed numeric code at this boundary.
                        bail!("Codex app-server rejected {method} (RPC {})", error.code);
                    }
                    return result.context("Codex app-server response omitted result");
                }
                IncomingMessage::Response { .. } => {
                    bail!("Codex app-server returned an unmatched response id")
                }
                IncomingMessage::Notification { method, params } => {
                    self.validate_notification(&method, &params, allowed_notifications)?;
                    self.queue_notification(method, params)?;
                }
                IncomingMessage::ServerRequest { id, method, params } => {
                    self.reject_server_request(id, &method).await?;
                    bail!(
                        "Codex app-server sent unsupported server request {method} ({})",
                        summarize_params(&params)
                    );
                }
            }
        }
    }

    fn validate_notification(
        &mut self,
        method: &str,
        params: &Value,
        allowed: &[&str],
    ) -> Result<()> {
        if !allowed.contains(&method) {
            bail!("Codex app-server sent unexpected notification {method}");
        }
        if method == "account/login/completed" {
            let login_id = required_string(params, "loginId")?;
            if !self.terminal_logins.insert(login_id) {
                bail!("Codex app-server sent duplicate login terminal notification");
            }
        }
        Ok(())
    }

    fn queue_notification(&mut self, method: String, params: Value) -> Result<()> {
        if self.queued.len() >= self.config.max_queued {
            bail!("Codex app-server notification queue exceeded the size limit");
        }
        self.queued.push_back((method, params));
        Ok(())
    }

    async fn reject_server_request(&mut self, id: Value, method: &str) -> Result<()> {
        self.send_value(&json!({
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Finch does not support server request {method}")
            }
        }))
        .await
    }

    async fn next_notification(&mut self, allowed: &[&str]) -> Result<(String, Value)> {
        if let Some(notification) = self.queued.pop_front() {
            if !allowed.contains(&notification.0.as_str()) {
                bail!(
                    "Codex app-server queued unexpected notification {}",
                    notification.0
                );
            }
            return Ok(notification);
        }
        loop {
            match self.read_message().await? {
                IncomingMessage::Notification { method, params } => {
                    self.validate_notification(&method, &params, allowed)?;
                    return Ok((method, params));
                }
                IncomingMessage::ServerRequest { id, method, params } => {
                    self.reject_server_request(id, &method).await?;
                    bail!(
                        "Codex app-server sent unsupported server request {method} ({})",
                        summarize_params(&params)
                    );
                }
                IncomingMessage::Response { .. } => {
                    bail!("Codex app-server returned an unexpected response")
                }
            }
        }
    }

    async fn read_message(&mut self) -> Result<IncomingMessage> {
        let mut frame = Vec::new();
        loop {
            let available = self
                .stdout
                .fill_buf()
                .await
                .context("Failed to read Codex app-server stdout")?;
            if available.is_empty() {
                let stderr = self.stderr_text();
                if frame.is_empty() {
                    bail!("Codex app-server exited unexpectedly{stderr}");
                }
                bail!("Codex app-server stdout ended with a truncated JSONL frame{stderr}");
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if frame.len().saturating_add(take) > self.config.max_frame_bytes {
                bail!("Codex app-server inbound message exceeded the size limit");
            }
            frame.extend_from_slice(&available[..take]);
            self.stdout.consume(take);
            if frame.last() == Some(&b'\n') {
                break;
            }
        }
        self.received_bytes = self.received_bytes.saturating_add(frame.len());
        self.received_messages = self.received_messages.saturating_add(1);
        if self.received_bytes > self.config.max_total_bytes
            || self.received_messages > self.config.max_messages
        {
            bail!("Codex app-server response stream exceeded aggregate limits");
        }
        let value = serde_json::from_slice(&frame).with_context(|| {
            format!(
                "Codex app-server returned malformed JSONL{}",
                self.stderr_text()
            )
        })?;
        decode_message(value)
    }

    fn stderr_text(&self) -> String {
        let text = self
            .stderr
            .lock()
            .expect("stderr capture lock poisoned")
            .redacted();
        if text.is_empty() {
            String::new()
        } else {
            format!("; stderr: {text}")
        }
    }

    async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        kill_process_group(&mut self.child);
        let _ = timeout(self.config.shutdown_timeout, self.child.wait()).await;
        if !self.stderr_task.is_finished() {
            self.stderr_task.abort();
        }
    }
}

impl Drop for JsonlTransport {
    fn drop(&mut self) {
        kill_process_group(&mut self.child);
        self.stderr_task.abort();
    }
}

fn summarize_params(params: &Value) -> &'static str {
    if params.is_object() {
        "object params"
    } else if params.is_array() {
        "array params"
    } else {
        "scalar params"
    }
}

/// One initialized stable-protocol app-server connection.
pub struct AppServerController {
    transport: JsonlTransport,
}

impl AppServerController {
    /// Spawn and complete the stable `initialize` → `initialized` handshake.
    pub async fn connect(config: AppServerConfig) -> Result<Self> {
        let mut transport = JsonlTransport::spawn(config).await?;
        if let Err(error) = transport.initialize().await {
            transport.shutdown().await;
            return Err(error);
        }
        Ok(Self { transport })
    }

    /// Read the current account without exposing credential material.
    pub async fn read_account(&mut self, refresh: bool) -> Result<ChatGptAccountStatus> {
        let result = self
            .transport
            .request_with_notifications(
                "account/read",
                json!({ "refreshToken": refresh }),
                &["account/updated"],
            )
            .await?;
        ChatGptAccountStatus::from_rpc(&result)
    }

    /// Begin the app-server-owned ChatGPT device-code flow.
    pub async fn start_device_login(mut self) -> Result<DeviceLoginSession> {
        let result = self
            .transport
            .request_with_notifications(
                "account/login/start",
                json!({ "type": "chatgptDeviceCode" }),
                &["account/login/completed", "account/updated"],
            )
            .await?;
        let details = DeviceCodeLogin {
            login_id: required_string(&result, "loginId")?,
            verification_url: required_string(&result, "verificationUrl")?,
            user_code: SecretText(required_string(&result, "userCode")?),
        };
        Ok(DeviceLoginSession {
            controller: self,
            details,
        })
    }

    /// Log out through app-server. Finch never deletes or inspects token files.
    pub async fn logout(&mut self) -> Result<()> {
        self.transport
            .request_with_notifications("account/logout", json!({}), &["account/updated"])
            .await?;
        Ok(())
    }

    /// Read all model pages and require GPT-5.6 Sol to be returned for this account.
    pub async fn list_models_requiring_sol(&mut self) -> Result<ModelCatalog> {
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        for _ in 0..MAX_MODEL_PAGES {
            let mut params = json!({ "limit": MODEL_PAGE_LIMIT, "includeHidden": false });
            if let Some(value) = &cursor {
                params["cursor"] = json!(value);
            }
            let result = self
                .transport
                .request_with_notifications("model/list", params, &["account/updated"])
                .await?;
            let page: ModelPage = serde_json::from_value(result)
                .context("Codex app-server returned an invalid model/list page")?;
            models.extend(page.data);
            if models.len() > MAX_MODELS {
                bail!("Codex app-server model catalog exceeded the size limit");
            }
            match page.next_cursor.filter(|value| !value.is_empty()) {
                Some(next) if seen_cursors.insert(next.clone()) => cursor = Some(next),
                Some(_) => bail!("Codex app-server model pagination repeated a cursor"),
                None => {
                    if !models.iter().any(CodexModel::is_sol) {
                        bail!("GPT-5.6 Sol is not available to the signed-in ChatGPT account");
                    }
                    return Ok(ModelCatalog { models });
                }
            }
        }
        bail!("Codex app-server model catalog exceeded the page limit")
    }

    /// Terminate the owned child process and its process group.
    pub async fn shutdown(mut self) {
        self.transport.shutdown().await;
    }
}

/// Account state safe to log and render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGptAccountStatus {
    pub signed_in: bool,
    pub plan_type: Option<String>,
}

impl ChatGptAccountStatus {
    fn from_rpc(result: &Value) -> Result<Self> {
        let account = result.get("account").filter(|value| !value.is_null());
        let signed_in = account
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            == Some("chatgpt");
        Ok(Self {
            signed_in,
            plan_type: signed_in
                .then(|| {
                    account
                        .and_then(|value| value.get("planType"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .flatten(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SecretText(String);

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// User-presentable data for one pending device-code login.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCodeLogin {
    pub login_id: String,
    pub verification_url: String,
    user_code: SecretText,
}

impl DeviceCodeLogin {
    /// The short-lived code the user must enter at `verification_url`.
    pub fn user_code(&self) -> &str {
        &self.user_code.0
    }
}

impl fmt::Debug for DeviceCodeLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCodeLogin")
            .field("login_id", &self.login_id)
            .field("verification_url", &self.verification_url)
            .field("user_code", &self.user_code)
            .finish()
    }
}

/// An app-server connection that owns one pending managed login.
pub struct DeviceLoginSession {
    controller: AppServerController,
    pub details: DeviceCodeLogin,
}

impl DeviceLoginSession {
    /// Wait for the correlated login completion notification.
    pub async fn wait(mut self) -> Result<AppServerController> {
        let deadline = Instant::now() + self.controller.transport.config.operation_timeout;
        wait_for_login_completion(
            &mut self.controller.transport,
            &self.details.login_id,
            deadline,
            None,
        )
        .await?;
        Ok(self.controller)
    }

    /// Cancel this login through the supported protocol and await its terminal notification.
    pub async fn cancel(mut self) -> Result<AppServerController> {
        self.controller
            .transport
            .request_with_notifications(
                "account/login/cancel",
                json!({ "loginId": &self.details.login_id }),
                &["account/login/completed", "account/updated"],
            )
            .await?;
        let deadline = Instant::now() + self.controller.transport.config.operation_timeout;
        wait_for_login_completion(
            &mut self.controller.transport,
            &self.details.login_id,
            deadline,
            Some(false),
        )
        .await?;
        Ok(self.controller)
    }
}

async fn wait_for_login_completion(
    transport: &mut JsonlTransport,
    login_id: &str,
    deadline: Instant,
    expected_success: Option<bool>,
) -> Result<()> {
    loop {
        let (method, params) = timeout_at(
            deadline,
            transport.next_notification(&["account/login/completed", "account/updated"]),
        )
        .await
        .context("Timed out waiting for ChatGPT login completion")??;
        if method == "account/updated" {
            continue;
        }
        if params.get("loginId").and_then(Value::as_str) != Some(login_id) {
            bail!("ChatGPT login completion did not match the pending login");
        }
        let success = params
            .get("success")
            .and_then(Value::as_bool)
            .context("ChatGPT login completion omitted success")?;
        if let Some(expected) = expected_success {
            if success != expected {
                bail!("ChatGPT login completion had an unexpected status");
            }
            return Ok(());
        }
        if success {
            return Ok(());
        }
        bail!("ChatGPT login did not complete successfully");
    }
}

/// One picker-visible model advertised by app-server.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<String>,
}

impl CodexModel {
    fn is_sol(&self) -> bool {
        !self.hidden && (self.id == GPT_5_6_SOL || self.model == GPT_5_6_SOL)
    }
}

fn default_input_modalities() -> Vec<String> {
    vec!["text".to_string(), "image".to_string()]
}

/// Reasoning option reported by `model/list`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    data: Vec<CodexModel>,
    next_cursor: Option<String>,
}

/// Fully paginated, account-scoped app-server model catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<CodexModel>,
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("Codex app-server omitted {field}"))
}

/// Text-only ChatGPT subscription provider backed by app-server.
#[derive(Clone)]
pub struct CodexAppServerProvider {
    config: AppServerConfig,
}

impl CodexAppServerProvider {
    /// Construct a provider from an explicit absolute executable path.
    pub fn new(executable: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            config: AppServerConfig::new(executable)?,
        })
    }

    /// Construct from an already validated controller configuration.
    pub fn with_config(config: AppServerConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl LlmProvider for CodexAppServerProvider {
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let _ = request;
        bail!("Codex app-server text turns are disabled: the stable official configuration contract does not provide a proven override that isolates all inherited hooks, plugins, skills, memories, MCP servers, and app tools")
    }

    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let _ = (&self.config, request);
        bail!("Codex app-server text turns are disabled: the stable official configuration contract does not provide a proven override that isolates all inherited hooks, plugins, skills, memories, MCP servers, and app tools")
    }

    fn name(&self) -> &str {
        "chatgpt_subscription"
    }

    fn default_model(&self) -> &str {
        GPT_5_6_SOL
    }

    fn supports_tools(&self) -> bool {
        false
    }

    fn context_limit_tokens(&self) -> usize {
        1_000_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn python() -> PathBuf {
        [
            "/usr/bin/python3",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .and_then(|path| std::fs::canonicalize(path).ok())
        .expect("python3 is required for the app-server fixture")
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex_app_server/fake_app_server.py")
    }

    fn test_config(scenario: &str) -> AppServerConfig {
        AppServerConfig::new(python())
            .unwrap()
            .with_prefix_args([fixture().display().to_string(), scenario.to_string()])
            .with_test_limits(1024, 64 * 1024, Duration::from_secs(2))
    }

    #[cfg(unix)]
    async fn assert_process_exits(pid: i32) {
        for _ in 0..100 {
            if unsafe { nix::libc::kill(pid, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("app-server descendant {pid} survived owner teardown");
    }

    #[cfg(unix)]
    async fn child_tree_config(scenario: &str) -> (AppServerConfig, tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("grandchild.pid");
        let config = AppServerConfig::new(python())
            .unwrap()
            .with_prefix_args([
                fixture().display().to_string(),
                scenario.to_string(),
                pid_path.display().to_string(),
            ])
            .with_test_limits(1024, 64 * 1024, Duration::from_secs(2));
        (config, directory, pid_path)
    }

    #[cfg(unix)]
    async fn read_fixture_pid(pid_path: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !pid_path.exists() && Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
        std::fs::read_to_string(pid_path).unwrap().parse().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn test_explicit_executable_rejects_relative_and_writable_paths() {
        assert!(AppServerConfig::new("codex").is_err());
        let directory = tempfile::tempdir().unwrap();
        let candidate = directory.path().join("codex");
        std::fs::write(&candidate, b"fixture").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = AppServerConfig::new(&candidate).unwrap_err();
        assert!(error.to_string().contains("group or world writable"));
    }

    #[cfg(unix)]
    #[test]
    fn test_executable_is_pinned_against_source_and_mutable_ancestor_replacement() {
        let outer = tempfile::tempdir().unwrap();
        let ancestor = outer.path().join("mutable");
        std::fs::create_dir(&ancestor).unwrap();
        let candidate = ancestor.join("codex");
        std::fs::write(&candidate, b"\x7fELForiginal-native-bytes").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = AppServerConfig::new(&candidate).unwrap();

        std::fs::rename(&ancestor, outer.path().join("replaced")).unwrap();
        std::fs::create_dir(&ancestor).unwrap();
        std::fs::write(&candidate, b"attacker-replacement").unwrap();
        assert_eq!(
            std::fs::read(config.executable()).unwrap(),
            b"\x7fELForiginal-native-bytes"
        );
        assert_eq!(
            std::fs::metadata(config.executable())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_executable_rejects_launcher_symlink_and_symlink_swap() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let native = directory.path().join("codex-native");
        let launcher = directory.path().join("codex");
        std::fs::write(&native, b"native").unwrap();
        std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&native, &launcher).unwrap();
        let error = AppServerConfig::new(&launcher).unwrap_err();
        assert!(error.to_string().contains("self-contained native binary"));
    }

    #[cfg(unix)]
    #[test]
    fn test_executable_rejects_regular_launcher_script_with_actionable_error() {
        let directory = tempfile::tempdir().unwrap();
        let launcher = directory.path().join("codex");
        std::fs::write(&launcher, b"#!/bin/sh\nexec sibling/codex\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = AppServerConfig::new(&launcher).unwrap_err();
        assert!(error.to_string().contains("native ELF or Mach-O binary"));
        assert!(error.to_string().contains("npm/Homebrew launcher script"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_keyring_environment_requires_safe_correlated_pair() {
        use std::ffi::OsString;
        let runtime = tempfile::tempdir().unwrap();
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let runtime_value = runtime.path().as_os_str().to_os_string();
        let bus_value = OsString::from(format!("unix:path={}/bus", runtime.path().display()));
        let inherited = validated_linux_keyring_environment(
            Some(runtime_value.clone()),
            Some(bus_value.clone()),
        )
        .unwrap();
        assert_eq!(inherited[0], ("XDG_RUNTIME_DIR", runtime_value.clone()));
        assert_eq!(inherited[1], ("DBUS_SESSION_BUS_ADDRESS", bus_value));

        assert!(validated_linux_keyring_environment(Some(runtime_value.clone()), None).is_err());
        assert!(validated_linux_keyring_environment(
            Some(runtime_value),
            Some(OsString::from("tcp:host=attacker.example")),
        )
        .is_err());
    }

    #[tokio::test]
    async fn test_stable_handshake_and_crlf_account_response() {
        let mut controller = AppServerController::connect(test_config("crlf"))
            .await
            .unwrap();
        let account = controller.read_account(false).await.unwrap();
        assert_eq!(
            account,
            ChatGptAccountStatus {
                signed_in: true,
                plan_type: Some("plus".to_string())
            }
        );
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_malformed_jsonl_fails_initialization() {
        let error = AppServerController::connect(test_config("malformed"))
            .await
            .err()
            .expect("malformed server unexpectedly initialized");
        assert!(error.to_string().contains("malformed JSONL"));
    }

    #[tokio::test]
    async fn test_oversized_jsonl_fails_before_allocation_growth() {
        let error = AppServerController::connect(test_config("oversized"))
            .await
            .err()
            .expect("oversized server unexpectedly initialized");
        assert!(error.to_string().contains("inbound message exceeded"));
    }

    #[tokio::test]
    async fn test_truncated_jsonl_is_not_parsed_as_complete() {
        let error = AppServerController::connect(test_config("truncated"))
            .await
            .err()
            .expect("truncated server unexpectedly initialized");
        assert!(error.to_string().contains("truncated JSONL frame"));
    }

    #[tokio::test]
    async fn test_invalid_utf8_jsonl_fails_initialization() {
        let error = AppServerController::connect(test_config("invalid_utf8"))
            .await
            .err()
            .expect("invalid UTF-8 server unexpectedly initialized");
        assert!(error.to_string().contains("malformed JSONL"));
    }

    #[tokio::test]
    async fn test_outbound_jsonl_is_bounded() {
        let mut controller = AppServerController::connect(test_config("normal"))
            .await
            .unwrap();
        controller.transport.config.max_frame_bytes = 64;
        let error = controller
            .transport
            .send_value(&json!({"method":"too/large","params":{"text":"x".repeat(200)}}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outbound message exceeded"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_stderr_is_fully_withheld_across_sensitive_and_split_content() {
        let mut config = test_config("noisy_stderr");
        config.max_stderr_bytes = 256;
        let mut controller = AppServerController::connect(config).await.unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("app-server stderr withheld"));
        assert!(rendered.contains("stderr truncated"));
        for secret in [
            "DO-NOT-LEAK",
            "Bearer",
            "eyJ",
            "Cookie",
            "password",
            "secret",
            "sk-",
            "?token=",
            "ordinary diagnostic",
        ] {
            assert!(!rendered.contains(secret));
        }
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_unexpected_notification_fails_closed() {
        let mut controller = AppServerController::connect(test_config("unexpected_notification"))
            .await
            .unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("unexpected notification plugin/started"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_inbound_aggregate_exhaustion_is_bounded() {
        let mut config = test_config("aggregate_exhaustion");
        config.max_total_bytes = 512;
        let mut controller = AppServerController::connect(config).await.unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        assert!(error.to_string().contains("aggregate limits"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_unknown_server_request_gets_error_response_and_fails_closed() {
        let mut controller = AppServerController::connect(test_config("unknown_request"))
            .await
            .unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported server request attestation/generate"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_initialize_hang_is_bounded_and_child_is_killed() {
        let mut config = test_config("hang_initialize");
        config.rpc_timeout = Duration::from_millis(150);
        config.shutdown_timeout = Duration::from_millis(150);
        let error = AppServerController::connect(config)
            .await
            .err()
            .expect("hung server unexpectedly initialized");
        assert!(error.to_string().contains("timed out during initialize"));
    }

    #[tokio::test]
    async fn test_crash_after_handshake_is_reported() {
        let mut controller = AppServerController::connect(test_config("crash_account"))
            .await
            .unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        assert!(error.to_string().contains("exited unexpectedly"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_device_login_correlates_completion_and_redacts_debug() {
        let controller = AppServerController::connect(test_config("login_success"))
            .await
            .unwrap();
        let login = controller.start_device_login().await.unwrap();
        assert_eq!(login.details.user_code(), "SECRET-CODE");
        let debug = format!("{:?}", login.details);
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("SECRET-CODE"));
        let controller = login.wait().await.unwrap();
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_device_login_rejects_mismatched_terminal() {
        let controller = AppServerController::connect(test_config("login_wrong_then_success"))
            .await
            .unwrap();
        let login = controller.start_device_login().await.unwrap();
        let error = login.wait().await.unwrap_err();
        assert!(error.to_string().contains("did not match"));
    }

    #[tokio::test]
    async fn test_device_login_denial_does_not_surface_server_error() {
        let controller = AppServerController::connect(test_config("login_denied"))
            .await
            .unwrap();
        let login = controller.start_device_login().await.unwrap();
        let error = login.wait().await.unwrap_err();
        assert!(error.to_string().contains("did not complete successfully"));
        assert!(!error.to_string().contains("expired secret details"));
    }

    #[tokio::test]
    async fn test_duplicate_late_login_terminal_is_rejected() {
        let controller = AppServerController::connect(test_config("login_duplicate_late"))
            .await
            .unwrap();
        let login = controller.start_device_login().await.unwrap();
        let mut controller = login.wait().await.unwrap();
        let error = controller.read_account(false).await.unwrap_err();
        assert!(error.to_string().contains("duplicate login terminal"));
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_device_login_cancel_awaits_correlated_terminal_state() {
        let controller = AppServerController::connect(test_config("login_pending"))
            .await
            .unwrap();
        let login = controller.start_device_login().await.unwrap();
        let controller = login.cancel().await.unwrap();
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_logout_uses_managed_app_server_account_api() {
        let mut controller = AppServerController::connect(test_config("normal"))
            .await
            .unwrap();
        controller.logout().await.unwrap();
        controller.shutdown().await;
    }

    #[tokio::test]
    async fn test_model_list_paginates_and_requires_sol() {
        let mut controller = AppServerController::connect(test_config("normal"))
            .await
            .unwrap();
        let catalog = controller.list_models_requiring_sol().await.unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert!(catalog.models.iter().any(CodexModel::is_sol));
        controller.shutdown().await;

        let mut controller = AppServerController::connect(test_config("sol_absent"))
            .await
            .unwrap();
        let error = controller.list_models_requiring_sol().await.unwrap_err();
        assert!(error.to_string().contains("Sol is not available"));
        controller.shutdown().await;

        for scenario in ["hidden_sol", "cursor_cycle"] {
            let mut controller = AppServerController::connect(test_config(scenario))
                .await
                .unwrap();
            let error = controller.list_models_requiring_sol().await.unwrap_err();
            if scenario == "hidden_sol" {
                assert!(error.to_string().contains("Sol is not available"));
            } else {
                assert!(error.to_string().contains("repeated a cursor"));
            }
            controller.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_text_provider_fails_before_spawning_app_server() {
        let provider = CodexAppServerProvider::with_config(test_config("text_turn"));
        let request = ProviderRequest::new(Vec::new()).with_model(GPT_5_6_SOL);
        let error = provider.send_message(&request).await.unwrap_err();
        assert!(error.to_string().contains("text turns are disabled"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_drop_controller_kills_owned_process_group() {
        let (config, _directory, pid_path) = child_tree_config("child_tree").await;
        let controller = AppServerController::connect(config).await.unwrap();
        let pid = read_fixture_pid(&pid_path).await;
        drop(controller);
        assert_process_exits(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_drop_pending_login_kills_owned_process_group() {
        let (config, _directory, pid_path) = child_tree_config("child_tree").await;
        let controller = AppServerController::connect(config).await.unwrap();
        let login = controller.start_device_login().await.unwrap();
        let pid = read_fixture_pid(&pid_path).await;
        drop(login);
        assert_process_exits(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_child_exit_first_still_cleans_descendants() {
        let (config, _directory, pid_path) = child_tree_config("child_exits_first").await;
        let mut controller = AppServerController::connect(config).await.unwrap();
        let pid = read_fixture_pid(&pid_path).await;
        let _ = controller.read_account(false).await.unwrap_err();
        drop(controller);
        assert_process_exits(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_shutdown_kills_owned_process_group() {
        let (config, _directory, pid_path) = child_tree_config("child_tree").await;
        let controller = AppServerController::connect(config).await.unwrap();
        let pid = read_fixture_pid(&pid_path).await;
        controller.shutdown().await;
        assert_process_exits(pid).await;
    }
}
