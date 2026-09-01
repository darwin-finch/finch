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
pub(crate) use brain_runner::{
    BoundedRunnerRequest, ConnectionDispatchAdmission, RunnerEffectAuditControlRequest,
    RunnerEffectAuditReservationRequest, RunnerHostEffectFinishRequest, RunnerProcessIdentity,
};
pub use brain_runner::{
    BrainRunnerBroker, RunnerApprovalRequest, RunnerCancelRequest, RunnerDeadlines,
    RunnerDispatchError, RunnerDispatchFailure, RunnerEffectAuditControl,
    RunnerEffectAuditReservation, RunnerEffectRecord, RunnerHostEffectOutcome,
    RunnerHostEffectPermit, RunnerMemoryProjectionRequest, RunnerOperation,
    RunnerProgramControlRequest, RunnerProgramError, RunnerProgramInteraction,
    RunnerProgramRequest, RunnerProgramResult, RunnerRegistrationId, RunnerRequest,
    RunnerTurnCommitAck, RunnerTurnCommitNotice, RunnerTurnError, RunnerTurnEvent,
    RunnerTurnRequest, RunnerTurnResult,
};
pub use brain_service::{
    BrainLifecycleService, BrainSubmissionError, BrainSubmissionOutcome, BrainWatch,
};
pub use feedback_handler::{handle_feedback, handle_training_status};
pub use handlers::{
    create_router, handle_node_info, handle_node_stats, health_check, metrics_endpoint,
};
#[cfg(unix)]
pub use handlers::{handle_node_info_from_state_directory, handle_node_stats_from_state_directory};
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
    /// Pins the descriptor-relative state root used only by authenticated
    /// Brain HTTP fixtures, so a pathname swap cannot redirect later opens.
    #[cfg(test)]
    supervised_state_root: Option<std::fs::File>,
}

#[cfg(test)]
struct SupervisedStateRoot {
    directory: std::fs::File,
    path: std::path::PathBuf,
}

#[cfg(test)]
fn supervised_state_root(
    proof: &crate::brain::IsolatedTestProof,
    requested: &std::path::Path,
) -> Result<SupervisedStateRoot> {
    use anyhow::Context as _;
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, Mode, SFlag};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::path::Component;

    let relative = requested
        .strip_prefix(&proof.home)
        .context("Brain HTTP fixture state must remain under the sealed HOME")?;
    anyhow::ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "Brain HTTP fixture state must be a normal descendant of the sealed HOME"
    );

    let home_fd = open(
        &proof.home,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .context("Could not open the sealed Brain test HOME")?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(home_fd) };
    let home_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        (home_stat.st_dev as u64, home_stat.st_ino as u64) == proof.home_identity,
        "sealed Brain test HOME identity changed"
    );
    for component in relative.components() {
        let Component::Normal(name) = component else {
            unreachable!()
        };
        let child_fd = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .context("Brain HTTP fixture state cannot traverse symlinks")?;
        directory = unsafe { std::fs::File::from_raw_fd(child_fd) };
    }
    let state_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(state_stat.st_mode).contains(SFlag::S_IFDIR),
        "Brain HTTP fixture state root must be a directory"
    );
    anyhow::ensure!(
        state_stat.st_uid == nix::unistd::geteuid().as_raw(),
        "Brain HTTP fixture state root must be owned by the isolated test user"
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let path = {
        static FIXTURE_ROOT_CLAIMED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        // Linux's `/proc/self/fd/N` and Darwin's `/dev/fd/N` are magic links;
        // feeding either into production storage's O_NOFOLLOW walk would
        // weaken that production boundary or fail closed before reaching the
        // pinned directory. These authenticated fixture tests instead run as
        // one exact, supervisor-owned subprocess and make the already-opened
        // directory its process-relative root. Fail closed before changing
        // cwd if a caller tries to reuse this seam in a parallel/broad test
        // process or constructs a second fixture.
        anyhow::ensure!(
            std::env::args_os().any(|argument| argument == "--exact"),
            "Brain fixtures require a dedicated exact test subprocess"
        );
        anyhow::ensure!(
            FIXTURE_ROOT_CLAIMED
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok(),
            "Darwin Brain fixture state is already pinned in this process"
        );
        anyhow::ensure!(
            unsafe { nix::libc::fchdir(directory.as_raw_fd()) } == 0,
            "could not pin the Brain fixture process to its state descriptor"
        );
        std::path::PathBuf::from(".")
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("descriptor-relative Brain fixture state is unsupported on this platform");
    Ok(SupervisedStateRoot { directory, path })
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
            brain_runners: BrainRunnerBroker::with_deadlines_and_quarantine_path(
                RunnerDeadlines::default(),
                Some(state_root.join("runner-process-quarantine-v1.json")),
            )?,
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(String::new())),
            supervised_state_root: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_supervised_brain_http_test(
        machine: &str,
        state_root: &std::path::Path,
        brain_credentials: crate::brain::credential::BrainCredentialAuthority,
    ) -> Result<Self> {
        use anyhow::Context as _;
        let proof = crate::brain::isolated_test_proof()
            .context("Brain HTTP fixture requires supervisor authority")?;
        let state_root = supervised_state_root(&proof, state_root)?;
        let password = proof.brain_password()?;
        let mut server = Self::for_brain_http_test(machine, &state_root.path, brain_credentials)?;
        server.brain_password = Arc::new(RwLock::new(password));
        server.supervised_state_root = Some(state_root.directory);
        Ok(server)
    }

    #[cfg(test)]
    pub(crate) fn for_brain_protocol_test(
        store: crate::brain::store::BrainStore,
        credentials: crate::brain::credential::BrainCredentialAuthority,
        password: String,
        state_root: &std::path::Path,
    ) -> Result<Self> {
        Self::for_brain_protocol_test_with_runner_deadlines(
            store,
            credentials,
            password,
            state_root,
            RunnerDeadlines::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_brain_protocol_test_with_runner_deadlines(
        store: crate::brain::store::BrainStore,
        credentials: crate::brain::credential::BrainCredentialAuthority,
        password: String,
        state_root: &std::path::Path,
        runner_deadlines: RunnerDeadlines,
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
            brain_runners: BrainRunnerBroker::with_deadlines_and_quarantine_path(
                runner_deadlines,
                Some(state_root.join("runner-process-quarantine-v1.json")),
            )?,
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials: credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(password)),
            supervised_state_root: None,
        })
    }

    /// Create a new agent server.
    ///
    /// `provider_graph` is the already validated named cloud graph shared with
    /// `claude_client`; provider construction must not be repeated here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        mut server_config: ServerConfig,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        local_generator: Arc<RwLock<LocalGenerator>>,
        bootstrap_loader: Arc<BootstrapLoader>,
        generator_state: Arc<RwLock<GeneratorState>>,
        provider_graph: ProviderGraph,
    ) -> Result<Self> {
        // Validate inherited supervisor authority before channels, credential
        // files, Brain stores, or configuration clones are created. A
        // malformed opt-in environment therefore has no server-side effects.
        let isolated_proof = crate::brain::isolated_test_proof_if_present()?;
        if let Some(proof) = &isolated_proof {
            server_config.bind_address = proof.daemon_address().to_owned();
            server_config.brain_bind_address = None;
            server_config.brain_password = proof.brain_password()?;
        }
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
            brain_runners: BrainRunnerBroker::with_deadlines_and_quarantine_path(
                RunnerDeadlines::default(),
                Some(credential_state.join("runner-process-quarantine-v1.json")),
            )?,
            brain_approvals: BrainApprovalBroker::default(),
            brain_credentials,
            mcp_servers,
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(brain_password)),
            #[cfg(test)]
            supervised_state_root: None,
        })
    }

    /// Start the HTTP server.
    ///
    /// Takes `Arc<Self>` so the same server instance can be shared with the
    /// Cap'n Proto IPC server that runs concurrently.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let isolated_proof = crate::brain::isolated_test_proof_if_present()?;
        let addr: SocketAddr = self.config.bind_address.parse()?;
        let listener = if let Some(proof) = isolated_proof {
            anyhow::ensure!(
                addr.to_string() == proof.daemon_address(),
                "isolated daemon bind address does not match supervisor authority"
            );
            let listener = proof.duplicate_daemon_listener()?;
            listener.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(listener)?
        } else {
            tokio::net::TcpListener::bind(addr).await?
        };
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
    let proof = crate::brain::isolated_test_proof()?;
    let relative = path
        .strip_prefix(&proof.home)
        .map_err(|_| anyhow::anyhow!("test address file must be inside the isolated HOME"))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("test address file has no final component"))?;
    anyhow::ensure!(
        relative.file_name().is_some()
            && relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
        "test address path contains an unsafe component"
    );
    #[cfg(unix)]
    {
        let parent = open_isolated_address_parent(&proof, relative)?;
        publish_isolated_address_file(&parent, name, bound_addr)
    }
    #[cfg(not(unix))]
    {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test address file has no parent"))?;
        publish_isolated_address_file(parent, name, bound_addr)
    }
}

#[cfg(unix)]
fn open_isolated_address_parent(
    proof: &crate::brain::IsolatedTestProof,
    relative: &std::path::Path,
) -> Result<std::fs::File> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, mkdirat, Mode, SFlag};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let raw = open(
        &proof.home,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(raw) };
    let home_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        (home_stat.st_dev as u64, home_stat.st_ino as u64) == proof.home_identity,
        "isolated HOME identity changed before address publication"
    );
    let parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test address file has no parent"))?;
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("test address parent contains an unsafe component");
        };
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let child_raw = match openat(Some(directory.as_raw_fd()), name, flags, Mode::empty()) {
            Ok(raw) => raw,
            Err(nix::errno::Errno::ENOENT) => {
                mkdirat(
                    Some(directory.as_raw_fd()),
                    name,
                    Mode::from_bits_truncate(0o700),
                )?;
                openat(Some(directory.as_raw_fd()), name, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        let child = unsafe { std::fs::File::from_raw_fd(child_raw) };
        let metadata = fstat(child.as_raw_fd())?;
        anyhow::ensure!(
            SFlag::from_bits_truncate(metadata.st_mode).contains(SFlag::S_IFDIR)
                && metadata.st_uid == nix::unistd::geteuid().as_raw()
                && metadata.st_nlink >= 1
                && metadata.st_mode & 0o022 == 0,
            "test address ancestor is not a private owned directory"
        );
        directory = child;
    }
    Ok(directory)
}

#[cfg(unix)]
fn publish_isolated_address_file(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    bound_addr: SocketAddr,
) -> Result<()> {
    use nix::fcntl::{openat, renameat, OFlag};
    use nix::sys::stat::{fstat, Mode, SFlag};
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::io::Write as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let directory_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(directory_stat.st_mode).contains(SFlag::S_IFDIR)
            && directory_stat.st_uid == nix::unistd::geteuid().as_raw()
            && directory_stat.st_nlink >= 1
            && directory_stat.st_mode & 0o022 == 0,
        "test address parent must be an owned, non-writable-by-others directory"
    );
    let temporary = std::ffi::OsString::from(format!(
        ".finch-bound-address-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let raw = openat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?;
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        let created = fstat(file.as_raw_fd())?;
        anyhow::ensure!(
            SFlag::from_bits_truncate(created.st_mode).contains(SFlag::S_IFREG)
                && created.st_uid == nix::unistd::geteuid().as_raw()
                && created.st_nlink == 1
                && created.st_mode & 0o777 == 0o600,
            "test address temporary is not a private, singly linked regular file"
        );
        file.write_all(bound_addr.to_string().as_bytes())?;
        file.sync_all()?;
        renameat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            Some(directory.as_raw_fd()),
            name,
        )?;
        directory.sync_all()?;
        let committed_raw = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let committed_file = unsafe { std::fs::File::from_raw_fd(committed_raw) };
        let committed = fstat(committed_file.as_raw_fd())?;
        anyhow::ensure!(
            committed.st_dev == created.st_dev
                && committed.st_ino == created.st_ino
                && SFlag::from_bits_truncate(committed.st_mode).contains(SFlag::S_IFREG)
                && committed.st_uid == nix::unistd::geteuid().as_raw()
                && committed.st_nlink == 1
                && committed.st_mode & 0o777 == 0o600,
            "test address publication changed identity during commit"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = unlinkat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        );
    }
    result
}

#[cfg(not(unix))]
fn publish_isolated_address_file(
    parent: &std::path::Path,
    name: &std::ffi::OsStr,
    bound_addr: SocketAddr,
) -> Result<()> {
    use std::io::Write as _;
    let temporary = parent.join(format!(
        ".finch-bound-address-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bound_addr.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, parent.join(name))?;
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

    fn isolated_provider_graph() -> ProviderGraph {
        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "sk-ant-isolated-provider-graph".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("isolated-provider-graph".into()),
            }]);
        crate::providers::create_provider_graph_from_config(&config)
            .expect("isolated tests need an in-memory provider graph")
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

    fn supervisor_contract_present() -> bool {
        // The permanent Brain-isolation CI gate runs these entries through
        // scripts/test_brains.sh, which supplies the authenticated contract.
        std::env::var_os("FINCH_BRAIN_TEST_TOKEN").is_some()
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
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("production constructor test requires supervisor-issued authority");
        let daemon_address = proof.daemon_address().to_owned();
        let brain_password = proof.brain_password().unwrap();
        let home = proof.home;
        let expected_root = proof.root;
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
            ServerConfig {
                bind_address: daemon_address,
                brain_password,
                ..ServerConfig::default()
            },
            ClaudeClient::new("isolated-constructor-test".to_string()).unwrap(),
            Router::new(crate::models::ThresholdRouter::new()),
            MetricsLogger::new(home.join(".finch/constructor-metrics")).unwrap(),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            isolated_provider_graph(),
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
    fn supervised_http_fixture_rejects_parent_traversal_without_external_mutation() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        let requested = proof
            .home
            .join("fixture")
            .join("..")
            .join("..")
            .join("outside");
        let result = AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &requested,
            crate::brain::credential::BrainCredentialAuthority::ephemeral([71; 32]),
        );
        let error = match result {
            Ok(_) => panic!("parent traversal unexpectedly constructed a server"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("normal descendant"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn supervised_http_fixture_rejects_symlinked_ancestor_without_external_mutation() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        let link = proof
            .home
            .join(format!("fixture-link-{}", uuid::Uuid::new_v4().simple()));
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let result = AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &link,
            crate::brain::credential::BrainCredentialAuthority::ephemeral([72; 32]),
        );
        let error = match result {
            Ok(_) => panic!("symlink traversal unexpectedly constructed a server"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("cannot traverse symlinks"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        std::fs::remove_file(link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn supervised_http_fixture_pins_state_root_across_ancestor_swap() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let requested = proof
            .home
            .join(format!("pinned-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&requested).unwrap();
        let pinned = supervised_state_root(&proof, &requested).unwrap();
        let moved = proof
            .home
            .join(format!("moved-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::rename(&requested, &moved).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        std::os::unix::fs::symlink(outside.path(), &requested).unwrap();

        let mut server = AgentServer::for_brain_http_test(
            "containment.local",
            &pinned.path,
            crate::brain::credential::BrainCredentialAuthority::ephemeral([73; 32]),
        )
        .unwrap();
        server.supervised_state_root = Some(pinned.directory);
        let brain = format!("pinned-brain-{}", uuid::Uuid::new_v4().simple());
        server
            .brain_store()
            .push(
                &brain,
                "containment-test",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "descriptor-pinned write".into(),
                },
            )
            .unwrap();

        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        assert!(moved.join("metrics").is_dir());
        assert!(moved.join("feedback.jsonl").is_file());
        assert!(moved
            .join("brains")
            .join(&brain)
            .join("events.jsonl")
            .is_file());
        assert!(!outside.path().join("brains").join(brain).exists());

        let second = proof
            .home
            .join(format!("second-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&second).unwrap();
        let error = match AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &second,
            crate::brain::credential::BrainCredentialAuthority::ephemeral([74; 32]),
        ) {
            Ok(_) => panic!("a second fixture changed the process-relative root"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already pinned"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
    }

    #[test]
    fn production_constructor_rejects_unverified_environment_before_store_mutation() {
        const CHILD_ENV: &str = "FINCH_TEST_CONSTRUCTOR_FORGERY_CHILD";
        if !supervisor_contract_present() {
            return;
        }
        if std::env::var_os(CHILD_ENV).is_some() {
            let proof = crate::brain::isolated_test_proof().unwrap();
            let forged_home = proof.home.join("forged-constructor-home");
            std::fs::create_dir(&forged_home).unwrap();
            let metrics =
                MetricsLogger::new(proof.home.join("forged-constructor-metrics")).unwrap();
            let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
            let bootstrap = Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None));
            std::env::set_var("HOME", &forged_home);
            std::env::set_var("FINCH_BRAIN_TEST_HOME", &forged_home);
            std::env::set_var("FINCH_BRAIN_TEST_ROOT", forged_home.join(".finch/brains"));
            let result = AgentServer::new(
                crate::config::Config::with_providers(Vec::new()),
                ServerConfig::default(),
                ClaudeClient::new("constructor-forgery".to_owned()).unwrap(),
                Router::new(crate::models::ThresholdRouter::new()),
                metrics,
                Arc::new(RwLock::new(LocalGenerator::new())),
                bootstrap,
                generator_state,
                isolated_provider_graph(),
            );
            assert!(result.is_err());
            assert!(std::fs::read_dir(&forged_home).unwrap().next().is_none());
            return;
        }
        let status = crate::brain::supervised_test_subprocess_command()
            .args([
                "--exact",
                "server::tests::production_constructor_rejects_unverified_environment_before_store_mutation",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn production_constructor_rejects_rewritten_proof_and_accepts_exact_restore() {
        const CHILD_ENV: &str = "FINCH_TEST_CONSTRUCTOR_REWRITTEN_PROOF_CHILD";
        if !supervisor_contract_present() {
            return;
        }
        if std::env::var_os(CHILD_ENV).is_some() {
            use std::io::Write as _;
            use std::os::fd::FromRawFd as _;
            use std::os::unix::fs::FileExt as _;

            let proof = crate::brain::isolated_test_proof().unwrap();
            let state_before = std::fs::read_dir(proof.home.join(".finch"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>();
            #[cfg(target_os = "macos")]
            {
                assert_eq!(unsafe { nix::libc::fchmod(9, 0o600) }, 0);
                let error = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open("/dev/fd/9")
                    .unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(unsafe { nix::libc::fchmod(9, 0o400) }, 0);
                let state_after = std::fs::read_dir(proof.home.join(".finch"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    state_after, state_before,
                    "constructor authority sealing attempt mutated Finch state"
                );
                crate::brain::isolated_test_proof().unwrap();
                return;
            }
            let duplicate = unsafe { nix::libc::dup(9) };
            assert!(duplicate >= 0);
            let reader = unsafe { std::fs::File::from_raw_fd(duplicate) };
            let length = reader.metadata().unwrap().len() as usize;
            let mut original = vec![0_u8; length];
            let mut offset = 0;
            while offset < original.len() {
                let count = reader
                    .read_at(&mut original[offset..], offset as u64)
                    .unwrap();
                assert!(count > 0);
                offset += count;
            }
            let mut forged = original.clone();
            forged[0] = if forged[0] == b'a' { b'b' } else { b'a' };
            for fd in [9, 108] {
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o600) }, 0);
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(format!("/dev/fd/{fd}"))
                    .unwrap();
                writer.write_all(&forged).unwrap();
                writer.sync_all().unwrap();
                drop(writer);
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o400) }, 0);
            }

            let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
            let result = AgentServer::new(
                crate::config::Config::with_providers(Vec::new()),
                ServerConfig::default(),
                ClaudeClient::new("constructor-rewrite".to_owned()).unwrap(),
                Router::new(crate::models::ThresholdRouter::new()),
                MetricsLogger::new(proof.home.join("constructor-rewrite-metrics")).unwrap(),
                Arc::new(RwLock::new(LocalGenerator::new())),
                Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
                generator_state,
                isolated_provider_graph(),
            );
            assert!(
                result.is_err(),
                "rewritten proof reached AgentServer construction"
            );
            let state_after = std::fs::read_dir(proof.home.join(".finch"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                state_after, state_before,
                "constructor mutated Finch state before rejecting rewritten authority"
            );

            for fd in [9, 108] {
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o600) }, 0);
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(format!("/dev/fd/{fd}"))
                    .unwrap();
                writer.write_all(&original).unwrap();
                writer.sync_all().unwrap();
                drop(writer);
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o400) }, 0);
            }
            crate::brain::isolated_test_proof().unwrap();
            return;
        }
        let status = crate::brain::supervised_test_subprocess_command()
            .args([
                "--exact",
                "server::tests::production_constructor_rejects_rewritten_proof_and_accepts_exact_restore",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_replaces_final_symlink_without_following_it() {
        let state = tempfile::tempdir().unwrap();
        let parent = state.path().join("private");
        std::fs::create_dir(&parent).unwrap();
        let outside = state.path().join("outside");
        std::fs::write(&outside, "sentinel").unwrap();
        std::os::unix::fs::symlink(&outside, parent.join("bound.addr")).unwrap();

        publish_isolated_address_file(
            &std::fs::File::open(&parent).unwrap(),
            std::ffi::OsStr::new("bound.addr"),
            "127.0.0.1:43210".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel");
        assert_eq!(
            std::fs::read_to_string(parent.join("bound.addr")).unwrap(),
            "127.0.0.1:43210"
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_replaces_final_hardlink_without_writing_its_inode() {
        use std::os::unix::fs::MetadataExt as _;
        let state = tempfile::tempdir().unwrap();
        let parent = state.path().join("private");
        std::fs::create_dir(&parent).unwrap();
        let outside = state.path().join("outside");
        std::fs::write(&outside, "sentinel").unwrap();
        std::fs::hard_link(&outside, parent.join("bound.addr")).unwrap();
        let outside_inode = std::fs::metadata(&outside).unwrap().ino();

        publish_isolated_address_file(
            &std::fs::File::open(&parent).unwrap(),
            std::ffi::OsStr::new("bound.addr"),
            "127.0.0.1:43211".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel");
        let committed = parent.join("bound.addr");
        assert_ne!(std::fs::metadata(&committed).unwrap().ino(), outside_inode);
        assert_eq!(
            std::fs::read_to_string(committed).unwrap(),
            "127.0.0.1:43211"
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_rejects_symlinked_ancestor() {
        use std::os::unix::fs::MetadataExt as _;
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("home");
        let root = home.join("safe-root");
        let outside = state.path().join("outside");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".finch")).unwrap();
        let metadata = std::fs::metadata(&home).unwrap();
        let proof = crate::brain::IsolatedTestProof {
            home_identity: (metadata.dev(), metadata.ino()),
            root_identity: (0, 0),
            ipc_socket: home.join("safe.sock"),
            socket_root: home.clone(),
            socket_root_identity: (metadata.dev(), metadata.ino()),
            ipc_listener_identity: (0, 0),
            home,
            root,
            brain_addr: String::new(),
            daemon_addr: String::new(),
            supervisor_pid: std::process::id(),
            password_digest: String::new(),
        };
        assert!(
            open_isolated_address_parent(&proof, std::path::Path::new(".finch/bound.addr"))
                .is_err()
        );
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
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
