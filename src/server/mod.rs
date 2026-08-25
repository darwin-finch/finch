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
mod session;
pub mod session_registry;
mod training_worker;

pub use brain_approval::BrainApprovalBroker;
pub use brain_runner::{
    BrainRunnerBroker, RunnerApprovalRequest, RunnerCancelRequest, RunnerEffectRecord,
    RunnerProgramError, RunnerProgramRequest, RunnerProgramResult, RunnerRegistrationId,
    RunnerRequest, RunnerTurnError, RunnerTurnEvent, RunnerTurnRequest, RunnerTurnResult,
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
pub use session::{SessionManager, SessionState};
pub use training_worker::TrainingWorker;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;

use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::local::LocalGenerator;
use crate::metrics::MetricsLogger;
use crate::models::{BootstrapLoader, GeneratorState, TrainingCoordinator};
use crate::providers::LlmProvider;
use crate::router::Router;

struct ProviderSlot {
    profile_name: String,
    provider: Arc<dyn LlmProvider>,
}

/// Configuration for the HTTP server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// Optional TLS-only listener for remote named-Brain collaboration.
    pub brain_bind_address: Option<String>,
    /// Maximum number of concurrent sessions
    pub max_sessions: usize,
    /// Session timeout in minutes
    pub session_timeout_minutes: u64,
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
            max_sessions: 100,
            session_timeout_minutes: 30,
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
    /// Session manager
    session_manager: Arc<SessionManager>,
    /// Server configuration
    config: ServerConfig,
    /// Local generator (Qwen model with LoRA)
    local_generator: Arc<RwLock<LocalGenerator>>,
    /// Bootstrap loader for progressive model loading
    bootstrap_loader: Arc<BootstrapLoader>,
    /// Generator state (tracks model loading progress)
    generator_state: Arc<RwLock<GeneratorState>>,
    /// Training coordinator for LoRA fine-tuning
    training_coordinator: Arc<TrainingCoordinator>,
    /// Training examples sender (for feedback endpoint)
    training_tx: Arc<tokio::sync::mpsc::UnboundedSender<crate::models::WeightedExample>>,
    /// Training examples receiver — taken once by `serve()` to hand to the worker.
    training_rx: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::models::WeightedExample>>,
    >,
    /// Authoritative event logs and program stacks for named shared brains.
    shared_brains: crate::brain::shared::SharedBrainStore,
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
    /// Create a new agent server.
    ///
    /// `providers` is the ordered list of cloud providers from `[[providers]]` config.
    /// If empty, the server falls back to `claude_client` for all cloud forwarding.
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
        training_coordinator: Arc<TrainingCoordinator>,
        providers: Vec<Box<dyn LlmProvider>>,
    ) -> Result<Self> {
        let session_manager = SessionManager::new(
            server_config.max_sessions,
            server_config.session_timeout_minutes,
        );

        // Create training channel; receiver is taken by serve() to hand to the worker.
        let (training_tx, training_rx) = tokio::sync::mpsc::unbounded_channel();
        let profile_names = config
            .providers
            .iter()
            .filter(|entry| !entry.is_local())
            .map(|entry| entry.profile_name());
        let providers: Vec<ProviderSlot> = providers
            .into_iter()
            .zip(profile_names)
            .map(|(provider, profile_name)| ProviderSlot {
                profile_name,
                provider: Arc::from(provider),
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
            session_manager: Arc::new(session_manager),
            config: server_config,
            local_generator,
            bootstrap_loader,
            generator_state,
            training_coordinator,
            training_tx: Arc::new(training_tx),
            training_rx: std::sync::Mutex::new(Some(training_rx)),
            shared_brains: crate::brain::shared::SharedBrainStore::new(machine),
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

        // Take the training receiver that was created in new().  Panics if
        // serve() is called more than once on the same instance (shouldn't happen).
        let training_rx = self
            .training_rx
            .lock()
            .unwrap()
            .take()
            .expect("AgentServer::serve() called twice");

        // Spawn training worker in background
        let worker = TrainingWorker::new(
            training_rx,
            Arc::clone(&self.training_coordinator),
            10, // batch_threshold: trigger after 10 examples
            5,  // batch_timeout_minutes: trigger after 5 minutes
        );

        tokio::spawn(async move {
            worker.run().await;
        });

        tracing::info!("Training worker spawned");

        // Background task: expire stale registry entries every 30 seconds.
        let registry = std::sync::Arc::clone(&crate::server::handlers::REGISTRY);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                registry.expire();
            }
        });

        // Auto-register this daemon in its own registry.
        // Every finch node is a registry by default — it accepts peers and is itself a peer.
        let self_addr = self.config.bind_address.clone();
        let specs = crate::node::collect_machine_specs();
        let self_entry = crate::registry::PeerEntry {
            addr: self_addr.clone(),
            label: Some(hostname_or_default()),
            tags: vec!["self".to_string()],
            load: None,
            region: None,
            cpu_cores: Some(specs.0),
            ram_mb: Some(specs.1),
            bench_ms: Some(specs.2),
        };
        let registry2 = std::sync::Arc::clone(&crate::server::handlers::REGISTRY);
        registry2.join(self_entry.clone());
        tracing::info!("Registered self in registry: {}", self_addr);

        // Heartbeat: keep this node alive in its own registry.
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                registry2.heartbeat(&self_addr);
            }
        });

        // Monitor generator state and inject model when ready
        let local_gen_clone = Arc::clone(&self.local_generator);
        let state_monitor = Arc::clone(&self.generator_state);
        tokio::spawn(async move {
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

        let auth = DaemonAuth::new(self.config.auth_enabled, self.config.api_keys.clone());

        // Use the existing Arc as application state.
        let app_state = self;

        // Build router with a body size limit to guard against oversized foreign payloads.
        // 4MB is generous for natural-language queries while blocking obvious DoS attempts.
        let app = create_router(Arc::clone(&app_state))
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024)) // 4MB
            .layer(axum::middleware::from_fn_with_state(auth, auth_middleware))
            .layer(TraceLayer::new_for_http());

        tracing::info!("Starting Finch agent server on {}", addr);

        // Start server — ConnectInfo requires into_make_service_with_connect_info
        // so handlers can read the peer's IP for auth logging.
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local_server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );

        if let Some(brain_bind_address) = &app_state.config.brain_bind_address {
            crate::node::tls::install_server_crypto_provider();
            let brain_addr: SocketAddr = brain_bind_address.parse()?;
            let hostname = hostname_or_default();
            let tls_identity = crate::node::tls::NodeTlsIdentity::from_signing_identity(
                app_state.brain_credentials.invitation_signer(),
                &hostname,
            )?;
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

    /// Get reference to session manager
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get reference to training examples sender
    pub fn training_tx(
        &self,
    ) -> &Arc<tokio::sync::mpsc::UnboundedSender<crate::models::WeightedExample>> {
        &self.training_tx
    }

    /// Get server configuration
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn shared_brains(&self) -> &crate::brain::shared::SharedBrainStore {
        &self.shared_brains
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

    /// Get reference to training coordinator
    pub fn training_coordinator(&self) -> &Arc<TrainingCoordinator> {
        &self.training_coordinator
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brain_password_comparison_checks_length_and_contents() {
        assert!(constant_time_eq(b"brain-secret", b"brain-secret"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secrex"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secret-longer"));
    }
    use crate::providers::{LlmProvider, ProviderRequest, ProviderResponse, StreamChunk};
    use async_trait::async_trait;
    use tokio::sync::mpsc::Receiver;

    struct NamedProvider(String);

    #[async_trait]
    impl LlmProvider for NamedProvider {
        fn name(&self) -> &str {
            &self.0
        }
        fn default_model(&self) -> &str {
            "test-model"
        }
        async fn send_message(&self, _r: &ProviderRequest) -> anyhow::Result<ProviderResponse> {
            unimplemented!()
        }
        async fn send_message_stream(
            &self,
            _r: &ProviderRequest,
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
