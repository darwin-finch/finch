// Shammah - Agent Server Module
// HTTP daemon mode for multi-tenant agent serving

mod brain_approval;
mod brain_runner;
mod brain_service;
mod feedback_handler;
pub mod handlers;
mod middleware;
mod openai_handlers;
pub mod openai_types; // Public for client access

pub use brain_approval::BrainApprovalBroker;
pub use brain_runner::{
    BrainRunnerBroker, RunnerApprovalRequest, RunnerCancelRequest, RunnerEffectRecord,
    RunnerMemoryProjectionRequest, RunnerProgramControlRequest, RunnerProgramError,
    RunnerProgramInteraction, RunnerProgramRequest, RunnerProgramResult, RunnerRegistrationId,
    RunnerRequest, RunnerTurnCommitAck, RunnerTurnCommitNotice, RunnerTurnError, RunnerTurnEvent,
    RunnerTurnRequest, RunnerTurnResult,
};
pub use brain_service::{
    BrainLifecycleService, BrainSubmissionError, BrainSubmissionOutcome, BrainWatch,
};
pub use feedback_handler::{handle_feedback, handle_training_status};
pub use handlers::{
    create_router, handle_node_info, handle_node_stats, health_check, metrics_endpoint,
};
pub use middleware::{auth_middleware, DaemonAuth, RateLimiter};
pub use openai_handlers::{handle_chat_completions, handle_list_models};
pub use openai_types::*;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;

use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::feedback::FeedbackLogger;
use crate::local::LocalGenerator;
use crate::metrics::MetricsLogger;
use crate::models::{BootstrapLoader, GeneratorState};
use crate::providers::{LlmProvider, ProviderGraph};
use crate::router::Router;

struct ProviderSlot {
    profile_name: String,
    provider: Arc<dyn LlmProvider>,
}

struct ServerBackgroundTasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for ServerBackgroundTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Configuration for the HTTP server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// Optional TLS-only listener for remote named-Brain collaboration.
    pub brain_bind_address: Option<String>,
    /// Enable API key authentication
    pub auth_enabled: bool,
    /// Valid API keys for authentication
    pub api_keys: Vec<String>,
    /// Password required for remote named-brain access.
    pub brain_password: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: crate::config::constants::DEFAULT_HTTP_ADDR.to_string(),
            brain_bind_address: None,
            auth_enabled: false,
            api_keys: vec![],
            brain_password: String::new(),
        }
    }
}

/// Main agent server structure
pub struct AgentServer {
    /// Claude API client (shared across sessions; kept for backward compat with handlers.rs)
    claude_client: Arc<ClaudeClient>,
    /// Multi-provider pool: cloud providers from [[providers]] config.
    /// Indexed by provider name for O(1) lookup via `provider_for_name()`.
    providers: Vec<ProviderSlot>,
    /// Router for decision-making (shared, read-write lock)
    router: Arc<RwLock<Router>>,
    /// Metrics logger (shared)
    metrics_logger: Arc<MetricsLogger>,
    /// Server configuration
    config: ServerConfig,
    /// Local generator (Qwen model with LoRA)
    local_generator: Arc<RwLock<LocalGenerator>>,
    /// Bootstrap loader for progressive model loading
    bootstrap_loader: Arc<BootstrapLoader>,
    /// Generator state (tracks model loading progress)
    generator_state: Arc<RwLock<GeneratorState>>,
    /// Append-only explicit user feedback. This is not a training queue.
    feedback_store: Arc<FeedbackLogger>,
    /// Authoritative event logs and program stacks for named shared brains.
    brain_store: crate::brain::store::BrainStore,
    /// Send-safe bridge to frontend-owned Cap'n Proto runner callbacks.
    brain_runners: BrainRunnerBroker,
    /// Pending approval continuations keyed to their exact Brain attachment.
    brain_approvals: BrainApprovalBroker,
    /// Persistent signer and revocation ledger for scoped remote participants.
    brain_credentials: crate::brain::credential::BrainCredentialAuthority,
    /// Application-owned MCP configuration and lazily connected transport for
    /// daemon-executed named-Brain programs. The transport is shared, while
    /// each Brain runtime installs its own verified vocabulary metadata.
    mcp_servers: std::collections::HashMap<String, crate::tools::mcp::McpServerConfig>,
    mcp_client: tokio::sync::OnceCell<Arc<crate::tools::mcp::McpClient>>,
    /// Runtime-rotatable password for remote named-brain access.
    brain_password: Arc<RwLock<String>>,
}

impl AgentServer {
    #[cfg(test)]
    pub(crate) fn for_brain_http_test(
        machine: &str,
        state_root: &std::path::Path,
        brain_credentials: crate::brain::credential::BrainCredentialAuthority,
    ) -> Result<Self> {
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        Ok(Self {
            claude_client: Arc::new(ClaudeClient::new(String::new())?),
            providers: Vec::new(),
            router: Arc::new(RwLock::new(Router::new(
                crate::models::ThresholdRouter::new(),
            ))),
            metrics_logger: Arc::new(MetricsLogger::new(state_root.join("metrics"))?),
            config: ServerConfig::default(),
            local_generator: Arc::new(RwLock::new(LocalGenerator::new())),
            bootstrap_loader: Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::at(state_root.join("feedback.jsonl"))?),
            brain_store: crate::brain::store::BrainStore::with_root(
                machine,
                Some(state_root.join("brains")),
            ),
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(String::new())),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_brain_protocol_test(
        store: crate::brain::store::BrainStore,
        credentials: crate::brain::credential::BrainCredentialAuthority,
        password: String,
        state_root: &std::path::Path,
    ) -> Result<Self> {
        let generator_state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let bootstrap_loader = Arc::new(BootstrapLoader::new(generator_state.clone(), None));
        Ok(Self {
            claude_client: Arc::new(ClaudeClient::new("brain-protocol-test".into())?),
            providers: Vec::new(),
            router: Arc::new(RwLock::new(Router::new(
                crate::models::ThresholdRouter::default(),
            ))),
            metrics_logger: Arc::new(MetricsLogger::new(state_root.join("metrics"))?),
            config: ServerConfig {
                brain_password: password.clone(),
                ..ServerConfig::default()
            },
            local_generator: Arc::new(RwLock::new(LocalGenerator::default())),
            bootstrap_loader,
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::at(state_root.join("feedback.jsonl"))?),
            brain_store: store,
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials: credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(password)),
        })
    }

    /// Create a new agent server.
    ///
    /// `provider_graph` is the already validated named cloud graph shared with
    /// `claude_client`; provider construction must not be repeated here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        server_config: ServerConfig,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        local_generator: Arc<RwLock<LocalGenerator>>,
        bootstrap_loader: Arc<BootstrapLoader>,
        generator_state: Arc<RwLock<GeneratorState>>,
        provider_graph: ProviderGraph,
    ) -> Result<Self> {
        let providers: Vec<ProviderSlot> = provider_graph
            .profiles()
            .iter()
            .map(|profile| ProviderSlot {
                profile_name: profile.profile_name().to_string(),
                provider: Arc::clone(profile.provider()),
            })
            .collect();

        let machine = hostname_or_default();
        let machine = if machine.contains('.') {
            machine
        } else {
            format!("{machine}.local")
        };
        let brain_password = server_config.brain_password.clone();
        let credential_state = dirs::home_dir()
            .ok_or_else(|| {
                anyhow::anyhow!("cannot initialize Brain credentials without a home directory")
            })?
            .join(".finch");
        let brain_credentials =
            crate::brain::credential::BrainCredentialAuthority::load_or_create(&credential_state)?;
        let mcp_servers = config.mcp_servers.clone();

        Ok(Self {
            claude_client: Arc::new(claude_client),
            providers,
            router: Arc::new(RwLock::new(router)),
            metrics_logger: Arc::new(metrics_logger),
            config: server_config,
            local_generator,
            bootstrap_loader,
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::new()?),
            brain_store: crate::brain::store::BrainStore::new(machine),
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials,
            mcp_servers,
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(brain_password)),
        })
    }

    /// Start the HTTP server.
    ///
    /// Takes `Arc<Self>` so the same server instance can be shared with the
    /// Cap'n Proto IPC server that runs concurrently.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let addr: SocketAddr = self.config.bind_address.parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.serve_on_listener(listener).await
    }

    async fn serve_on_listener(self: Arc<Self>, listener: tokio::net::TcpListener) -> Result<()> {
        let addr = listener.local_addr()?;

        // The daemon owns only due-time calculation and durable queueing.
        // Actual ProgramRuns remain on each Brain's leased environment runner.
        let schedule_store = self.brain_store.clone();
        let schedule_runners = self.brain_runners.clone();
        let schedule_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let names = match schedule_store.list() {
                    Ok(names) => names,
                    Err(error) => {
                        tracing::warn!(%error, "could not list Brains for schedule delivery");
                        continue;
                    }
                };
                for name in names {
                    if let Err(error) = handlers::deliver_due_named_brain_schedules(
                        schedule_store.clone(),
                        schedule_runners.clone(),
                        name.clone(),
                        crate::brain::store::unix_millis(),
                    )
                    .await
                    {
                        tracing::warn!(brain = %name, %error, "could not deliver due Brain schedule");
                    }
                }
            }
        });

        // Monitor generator state and inject model when ready
        let local_gen_clone = Arc::clone(&self.local_generator);
        let state_monitor = Arc::clone(&self.generator_state);
        let model_monitor_task = tokio::spawn(async move {
            tracing::info!("Model monitor task started");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                let state = state_monitor.read().await;
                tracing::debug!(
                    "Monitor checking state: {:?}",
                    std::mem::discriminant(&*state)
                );

                if let GeneratorState::Ready { model, model_name } = &*state {
                    let model_clone = Arc::clone(model);
                    let name = model_name.clone();
                    drop(state); // Release read lock before acquiring write lock

                    tracing::info!("Model is ready: {}, injecting into LocalGenerator", name);

                    // Try to inject with timeout
                    match tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
                        tracing::info!("Acquiring write lock on LocalGenerator...");
                        let mut gen = local_gen_clone.write().await;
                        tracing::info!("Write lock acquired, creating new LocalGenerator...");
                        *gen = LocalGenerator::with_models(Some(model_clone));
                        tracing::info!("LocalGenerator updated");
                    })
                    .await
                    {
                        Ok(_) => {
                            tracing::info!("✓ Model injected - local generation enabled");
                            break; // Stop monitoring once injected
                        }
                        Err(_) => {
                            tracing::error!(
                                "❌ Timeout while injecting model (5s) - write lock may be held"
                            );
                        }
                    }
                } else if matches!(
                    *state,
                    GeneratorState::Failed { .. } | GeneratorState::NotAvailable
                ) {
                    tracing::warn!("Model loading failed or not available, stopping monitor");
                    break; // Stop monitoring on failure
                }
            }
            tracing::info!("Model monitor task exiting");
        });
        let _background_tasks = ServerBackgroundTasks(vec![schedule_task, model_monitor_task]);

        let auth = DaemonAuth::new(self.config.auth_enabled, self.config.api_keys.clone());

        // Use the existing Arc as application state.
        let app_state = self;

        // Build router with a body size limit to guard against oversized foreign payloads.
        // 4MB is generous for natural-language queries while blocking obvious DoS attempts.
        let app = create_router(Arc::clone(&app_state))
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024)) // 4MB
            .layer(axum::middleware::from_fn_with_state(auth, auth_middleware))
            .layer(TraceLayer::new_for_http());

        // Start server — ConnectInfo requires into_make_service_with_connect_info
        // so handlers can read the peer's IP for auth logging.
        publish_isolated_test_address(addr)?;
        tracing::info!("Starting Finch agent server on {}", addr);
        let local_server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );

        if let Some(brain_bind_address) = &app_state.config.brain_bind_address {
            crate::node::tls::install_server_crypto_provider();
            let brain_addr: SocketAddr = brain_bind_address.parse()?;
            let tls_identity = app_state.brain_credentials.invitation_tls_identity();
            let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(
                vec![tls_identity.certificate_der().to_vec()],
                tls_identity.private_key_der().to_vec(),
            )
            .await?;
            let brain_app = crate::server::handlers::create_remote_brain_router(app_state)
                .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
                .layer(TraceLayer::new_for_http());
            tracing::info!("Starting encrypted Brain listener on {}", brain_addr);
            let remote_server = axum_server::bind_rustls(brain_addr, tls_config)
                .serve(brain_app.into_make_service_with_connect_info::<std::net::SocketAddr>());
            tokio::select! {
                result = local_server => result?,
                result = remote_server => result?,
            }
        } else {
            local_server.await?;
        }

        Ok(())
    }

    /// Get reference to Claude client
    pub fn claude_client(&self) -> &Arc<ClaudeClient> {
        &self.claude_client
    }

    /// Resolve the cloud provider to use for a given request.
    ///
    /// Names resolve configured profiles first. A provider type such as
    /// `openai` is accepted only when it identifies exactly one profile.
    /// `None` selects the first configured cloud profile.
    pub fn provider_for_name(&self, name: Option<&str>) -> Option<&Arc<dyn LlmProvider>> {
        if self.providers.is_empty() {
            return None;
        }
        if let Some(n) = name {
            if let Some(slot) = self
                .providers
                .iter()
                .find(|slot| slot.profile_name.eq_ignore_ascii_case(n))
            {
                return Some(&slot.provider);
            }

            let by_type: Vec<_> = self
                .providers
                .iter()
                .filter(|slot| slot.provider.name().eq_ignore_ascii_case(n))
                .collect();
            return match by_type.as_slice() {
                [slot] => Some(&slot.provider),
                _ => None,
            };
        }
        self.providers.first().map(|slot| &slot.provider)
    }

    /// Whether named cloud provider profiles are configured.
    ///
    /// The OpenAI-compatible API uses this to distinguish an unknown profile
    /// name from the legacy configuration path, which has no provider pool.
    pub fn has_provider_profiles(&self) -> bool {
        !self.providers.is_empty()
    }

    /// Get reference to router
    pub fn router(&self) -> &Arc<RwLock<Router>> {
        &self.router
    }

    /// Get reference to metrics logger
    pub fn metrics_logger(&self) -> &Arc<MetricsLogger> {
        &self.metrics_logger
    }

    /// Get the append-only explicit feedback store.
    pub fn feedback_store(&self) -> &Arc<FeedbackLogger> {
        &self.feedback_store
    }

    /// Get server configuration
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn brain_store(&self) -> &crate::brain::store::BrainStore {
        &self.brain_store
    }

    pub fn brain_runners(&self) -> &BrainRunnerBroker {
        &self.brain_runners
    }

    pub fn brain_approvals(&self) -> &BrainApprovalBroker {
        &self.brain_approvals
    }

    /// Return the daemon-owned MCP transport, connecting it on first use.
    /// Named Brain runtimes borrow this host service but retain independent
    /// typed dictionaries, manifests, grants, and effect journals.
    pub async fn mcp_client(&self) -> Result<Option<Arc<crate::tools::mcp::McpClient>>> {
        if self.mcp_servers.is_empty() {
            return Ok(None);
        }
        let client = self
            .mcp_client
            .get_or_try_init(|| async {
                crate::tools::mcp::McpClient::from_config(&self.mcp_servers)
                    .await
                    .map(Arc::new)
            })
            .await?;
        Ok(Some(Arc::clone(client)))
    }

    pub async fn brain_password(&self) -> String {
        self.brain_password.read().await.clone()
    }

    pub async fn check_brain_password(&self, candidate: &str) -> bool {
        constant_time_eq(
            self.brain_password.read().await.as_bytes(),
            candidate.as_bytes(),
        )
    }

    pub async fn set_brain_password(&self, password: String) {
        *self.brain_password.write().await = password;
    }

    pub fn brain_credentials(&self) -> &crate::brain::credential::BrainCredentialAuthority {
        &self.brain_credentials
    }

    /// Get reference to local generator
    pub fn local_generator(&self) -> &Arc<RwLock<LocalGenerator>> {
        &self.local_generator
    }

    /// Get reference to bootstrap loader
    pub fn bootstrap_loader(&self) -> &Arc<BootstrapLoader> {
        &self.bootstrap_loader
    }

    /// Get reference to generator state
    pub fn generator_state(&self) -> &Arc<RwLock<GeneratorState>> {
        &self.generator_state
    }

    /// Return the primary cloud provider (first in the configured list, if any).
    ///
    /// Used by the IPC server to service CLI queries without going through the
    /// full HTTP handler stack.
    pub fn primary_provider(&self) -> Option<Arc<dyn crate::providers::LlmProvider>> {
        self.providers
            .first()
            .map(|slot| Arc::clone(&slot.provider))
    }
}

fn publish_isolated_test_address(bound_addr: SocketAddr) -> Result<()> {
    let Some(path) = std::env::var_os("FINCH_TEST_BOUND_ADDR_FILE").map(std::path::PathBuf::from)
    else {
        return Ok(());
    };
    anyhow::ensure!(
        std::env::var_os("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Some(std::ffi::OsStr::new("1")),
        "FINCH_TEST_BOUND_ADDR_FILE requires the Brain isolation wrapper"
    );
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("test HOME is unavailable"))?;
    let home = home.canonicalize()?;
    let test_home = std::env::var_os("FINCH_BRAIN_TEST_HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("FINCH_BRAIN_TEST_HOME is unavailable"))?;
    anyhow::ensure!(
        test_home.canonicalize()? == home
            && std::env::var_os("FINCH_BRAIN_TEST_ROOT").as_deref()
                == Some(test_home.join(".finch/brains").as_os_str()),
        "test address publication requires the isolated HOME contract"
    );
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test address file has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    anyhow::ensure!(
        path.is_absolute() && parent.starts_with(&home),
        "test address file must be inside the isolated HOME"
    );
    std::fs::write(path, bound_addr.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    async fn submit_feedback_to_daemon(address: SocketAddr, query: &str) {
        let client = reqwest::Client::new();
        let endpoint = format!("http://{address}/v1/feedback");
        for _ in 0..20 {
            match client
                .post(&endpoint)
                .json(&serde_json::json!({
                    "query": query,
                    "response": "metadata-only response",
                    "weight": 3.0,
                    "feedback": "retain privately"
                }))
                .send()
                .await
            {
                Ok(response) => {
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    let status: serde_json::Value = client
                        .post(format!("http://{address}/v1/training/status"))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    assert_eq!(status["training_active"], false);
                    assert_eq!(status["queue_length"], 0);
                    return;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        panic!("feedback daemon did not accept a local request");
    }

    #[tokio::test(start_paused = true)]
    async fn test_daemon_feedback_timeout_and_restart_never_request_training_process() {
        let temp = tempfile::tempdir().unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let feedback_path = temp.path().join("feedback.jsonl");
        let legacy_queue = temp.path().join("training_queue.jsonl");
        let adapter = temp.path().join("adapters/latest.safetensors");
        let legacy_queue_contents = "legacy queued Python training example\n";
        std::fs::write(&legacy_queue, legacy_queue_contents).unwrap();
        let observed_launches = Arc::clone(&launches);
        let _launch_observer = crate::training::lora_subprocess::observe_training_process_launches(
            Arc::new(move || {
                observed_launches.fetch_add(1, Ordering::SeqCst);
            }),
        );

        for (cycle, query) in ["before restart", "after restart"].into_iter().enumerate() {
            let authority = crate::brain::credential::BrainCredentialAuthority::ephemeral(
                [cycle as u8 + 1; 32],
            );
            let server =
                AgentServer::for_brain_http_test("feedback-fixture.local", temp.path(), authority)
                    .unwrap();
            let server = Arc::new(server);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

            submit_feedback_to_daemon(address, query).await;
            tokio::time::advance(tokio::time::Duration::from_secs(10 * 60)).await;
            tokio::task::yield_now().await;

            assert_eq!(launches.load(Ordering::SeqCst), 0);
            assert_eq!(
                std::fs::read_to_string(&legacy_queue).unwrap(),
                legacy_queue_contents
            );
            assert!(!adapter.exists());
            serving.abort();
            let _ = serving.await;
        }

        let entries = FeedbackLogger::at(&feedback_path)
            .unwrap()
            .load_all()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].query, "before restart");
        assert_eq!(entries[1].query, "after restart");
        assert_eq!(entries[0].weight, 3.0);
        assert_eq!(entries[1].weight, 3.0);
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_daemon_feedback_storage_failure_redacts_private_path() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("customer-secret-project");
        let authority = crate::brain::credential::BrainCredentialAuthority::ephemeral([7; 32]);
        let mut server =
            AgentServer::for_brain_http_test("feedback-fixture.local", &state_root, authority)
                .unwrap();
        let feedback_path = state_root.join("feedback.jsonl");
        server.feedback_store = Arc::new(
            FeedbackLogger::at(&feedback_path)
                .unwrap()
                .with_injected_log_error(format!("failed at {}", feedback_path.display())),
        );
        let server = Arc::new(server);
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&captured_writer)))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

        let client = reqwest::Client::new();
        let mut response = None;
        for _ in 0..20 {
            match client
                .post(format!("http://{address}/v1/feedback"))
                .json(&serde_json::json!({
                    "query": "redact storage location",
                    "response": "metadata only",
                    "weight": 1.0
                }))
                .send()
                .await
            {
                Ok(result) => {
                    response = Some(result);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let response = response.expect("feedback daemon did not accept a local request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = response.text().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "status": "error",
                "message": "Could not persist feedback"
            })
        );
        assert!(!body.contains("customer-secret-project"));
        assert!(!body.contains(temp.path().to_string_lossy().as_ref()));

        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("Failed to persist private feedback"));
        assert!(logs.contains("storage-error"));
        assert!(!logs.contains("customer-secret-project"));
        assert!(!logs.contains(temp.path().to_string_lossy().as_ref()));

        serving.abort();
        let _ = serving.await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_daemon_feedback_quota_is_redacted_unchanged_and_never_trains() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("customer-secret-project");
        let feedback_path = state_root.join("feedback.jsonl");
        let legacy_queue = state_root.join("training_queue.jsonl");
        let adapter = state_root.join("adapters/latest.safetensors");
        let launches = Arc::new(AtomicUsize::new(0));
        let observed_launches = Arc::clone(&launches);
        let _launch_observer = crate::training::lora_subprocess::observe_training_process_launches(
            Arc::new(move || {
                observed_launches.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let authority = crate::brain::credential::BrainCredentialAuthority::ephemeral([8; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("feedback-fixture.local", &state_root, authority)
                .unwrap(),
        );
        std::fs::write(&legacy_queue, "legacy queued example\n").unwrap();
        let mut feedback = std::fs::OpenOptions::new()
            .write(true)
            .open(&feedback_path)
            .unwrap();
        feedback
            .set_len(crate::feedback::FEEDBACK_LOG_MAX_BYTES)
            .unwrap();
        feedback.seek(SeekFrom::End(-1)).unwrap();
        feedback.write_all(b"\n").unwrap();
        feedback.sync_all().unwrap();
        drop(feedback);
        let bytes_before = std::fs::read(&feedback_path).unwrap();

        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&captured_writer)))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

        let client = reqwest::Client::new();
        let mut response = None;
        for _ in 0..20 {
            match client
                .post(format!("http://{address}/v1/feedback"))
                .json(&serde_json::json!({
                    "query": "quota boundary",
                    "response": "metadata only",
                    "weight": 1.0
                }))
                .send()
                .await
            {
                Ok(result) => {
                    response = Some(result);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let response = response.expect("feedback daemon did not accept a local request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = response.text().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "status": "error",
                "message": "Could not persist feedback"
            })
        );
        tokio::time::advance(tokio::time::Duration::from_secs(10 * 60)).await;
        tokio::task::yield_now().await;

        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("Failed to persist private feedback"));
        assert!(logs.contains("quota-exceeded"));
        assert!(!body.contains("customer-secret-project"));
        assert!(!logs.contains("customer-secret-project"));
        assert!(!logs.contains(temp.path().to_string_lossy().as_ref()));
        assert_eq!(std::fs::read(&feedback_path).unwrap(), bytes_before);
        assert_eq!(
            std::fs::read_to_string(&legacy_queue).unwrap(),
            "legacy queued example\n"
        );
        assert!(!adapter.exists());
        assert_eq!(launches.load(Ordering::SeqCst), 0);

        serving.abort();
        let _ = serving.await;
    }

    fn isolated_http_server() -> (tempfile::TempDir, Arc<AgentServer>) {
        let state = tempfile::tempdir().unwrap();
        let credentials = crate::brain::credential::BrainCredentialAuthority::load_or_create(
            &state.path().join("credentials"),
        )
        .unwrap();
        let server =
            AgentServer::for_brain_http_test("test.local", state.path(), credentials).unwrap();
        (state, Arc::new(server))
    }

    fn isolated_http_router(server: Arc<AgentServer>) -> axum::Router {
        crate::server::handlers::create_router(server)
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
    }

    #[tokio::test]
    async fn production_router_health_is_hermetic() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let response = isolated_http_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn production_router_rejects_malformed_and_oversized_messages() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let malformed = isolated_http_router(Arc::clone(&server))
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"bad": json"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(malformed.status().as_u16(), 400 | 422));

        let oversized = isolated_http_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from("0".repeat(5 * 1024 * 1024)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            oversized.status(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn production_constructor_persists_named_brain_only_in_isolated_home() {
        assert_eq!(
            std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref(),
            Ok("1")
        );
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("Brain tests require scripts/test_brains.sh"),
        );
        let expected_root = std::path::PathBuf::from(
            std::env::var_os("FINCH_BRAIN_TEST_ROOT")
                .expect("Brain tests require an explicit isolated root"),
        );
        assert_eq!(expected_root, home.join(".finch/brains"));

        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "isolated-constructor-test".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("isolated-constructor-test".into()),
            }]);
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        let server = AgentServer::new(
            config,
            ServerConfig::default(),
            ClaudeClient::new("isolated-constructor-test".to_string()).unwrap(),
            Router::new(crate::models::ThresholdRouter::new()),
            MetricsLogger::new(home.join(".finch/constructor-metrics")).unwrap(),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            Arc::new(TrainingCoordinator::with_queue_path(
                4,
                4,
                false,
                home.join(".finch/constructor-training.jsonl"),
            )),
            Vec::new(),
        )
        .unwrap();
        let name = format!("constructor-boundary-{}", uuid::Uuid::new_v4().simple());
        server
            .brain_store()
            .push(
                &name,
                "isolation-test",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "boundary proof".into(),
                },
            )
            .unwrap();
        assert!(expected_root.join(name).join("events.jsonl").is_file());
    }

    #[test]
    fn brain_password_comparison_checks_length_and_contents() {
        assert!(constant_time_eq(b"brain-secret", b"brain-secret"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secrex"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secret-longer"));
    }
    use crate::providers::{
        LlmProvider, ProviderBackend, ProviderResponse, StreamChunk, ValidatedProviderRequest,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc::Receiver;

    struct NamedProvider(String);

    #[async_trait]
    impl ProviderBackend for NamedProvider {
        fn name(&self) -> &str {
            &self.0
        }
        fn default_model(&self) -> &str {
            "test-model"
        }
        async fn send_message_validated(
            &self,
            _r: ValidatedProviderRequest,
        ) -> anyhow::Result<ProviderResponse> {
            unimplemented!()
        }
        async fn send_message_stream_validated(
            &self,
            _r: ValidatedProviderRequest,
        ) -> anyhow::Result<Receiver<anyhow::Result<StreamChunk>>> {
            unimplemented!()
        }
    }

    fn make_providers(names: &[&str]) -> Vec<Arc<dyn LlmProvider>> {
        names
            .iter()
            .map(|n| Arc::new(NamedProvider(n.to_string())) as Arc<dyn LlmProvider>)
            .collect()
    }

    #[test]
    fn test_provider_for_name_found_exact() {
        // Build a minimal AgentServer-like providers Vec and call provider_for_name directly
        // (we test via a wrapper since building a full AgentServer requires many deps)
        let providers = make_providers(&["claude", "grok", "openai"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("grok"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "grok");
    }

    #[test]
    fn test_provider_for_name_case_insensitive() {
        let providers = make_providers(&["Claude", "Grok"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("claude"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "Claude");
    }

    #[test]
    fn test_provider_for_name_not_found_returns_none_when_empty() {
        let providers: Vec<Arc<dyn LlmProvider>> = vec![];
        // Mirrors provider_for_name: empty -> None
        let result = if providers.is_empty() {
            None
        } else {
            providers.first()
        };
        assert!(result.is_none());
    }

    #[test]
    fn test_provider_for_name_unknown_does_not_silently_fall_back() {
        let providers = make_providers(&["claude", "grok"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("unknown"));
        assert!(result.is_none());
    }

    #[test]
    fn test_provider_for_name_none_name_returns_first() {
        let providers = make_providers(&["claude", "grok"]);
        // None name → first provider
        let name: Option<&str> = None;
        let result = if providers.is_empty() {
            None
        } else if let Some(n) = name {
            providers
                .iter()
                .find(|p| p.name().eq_ignore_ascii_case(n))
                .or_else(|| providers.first())
        } else {
            providers.first()
        };
        assert_eq!(result.unwrap().name(), "claude");
    }
}

/// Return the OS hostname, or "finch-node" if it can't be determined.
fn hostname_or_default() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "finch-node".to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let width = left.len().max(right.len());
    for index in 0..width {
        diff |= left.get(index).copied().unwrap_or(0) as usize
            ^ right.get(index).copied().unwrap_or(0) as usize;
    }
    diff == 0
}
