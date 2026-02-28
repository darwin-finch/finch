// Shammah - Agent Server Module
// HTTP daemon mode for multi-tenant agent serving

pub mod brain_registry;
mod feedback_handler;
mod handlers;
mod middleware;
mod openai_handlers;
pub mod openai_types; // Public for client access
mod session;
mod training_worker;

pub use brain_registry::{BrainDetail, BrainRegistry, BrainState, BrainSummary, PlanResponse};
pub use feedback_handler::{handle_feedback, handle_training_status};
pub use handlers::{
    create_router, handle_node_info, handle_node_stats, health_check, metrics_endpoint,
};
pub use middleware::{auth_middleware, RateLimiter};
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

/// Configuration for the HTTP server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// Maximum number of concurrent sessions
    pub max_sessions: usize,
    /// Session timeout in minutes
    pub session_timeout_minutes: u64,
    /// Enable API key authentication
    pub auth_enabled: bool,
    /// Valid API keys for authentication
    pub api_keys: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: crate::config::constants::DEFAULT_HTTP_ADDR.to_string(),
            max_sessions: 100,
            session_timeout_minutes: 30,
            auth_enabled: false,
            api_keys: vec![],
        }
    }
}

/// Main agent server structure
pub struct AgentServer {
    /// Claude API client (shared across sessions; kept for backward compat with handlers.rs)
    claude_client: Arc<ClaudeClient>,
    /// Multi-provider pool: cloud providers from [[providers]] config.
    /// Indexed by provider name for O(1) lookup via `provider_for_name()`.
    providers: Vec<Arc<dyn LlmProvider>>,
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
    /// Brain registry — tracks all daemon brain sessions
    brain_registry: Arc<BrainRegistry>,
}

impl AgentServer {
    /// Create a new agent server.
    ///
    /// `providers` is the ordered list of cloud providers from `[[providers]]` config.
    /// If empty, the server falls back to `claude_client` for all cloud forwarding.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        _config: Config,
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

        // Create training channel (will be connected to worker in serve())
        let (training_tx, _training_rx) = tokio::sync::mpsc::unbounded_channel();
        let providers: Vec<Arc<dyn LlmProvider>> = providers.into_iter().map(Arc::from).collect();

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
            brain_registry: Arc::new(BrainRegistry::new()),
        })
    }

    /// Start the HTTP server
    pub async fn serve(mut self) -> Result<()> {
        let addr: SocketAddr = self.config.bind_address.parse()?;

        // Create training worker channel
        let (training_tx, training_rx) = tokio::sync::mpsc::unbounded_channel();
        self.training_tx = Arc::new(training_tx);

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

        // Create application state
        let app_state = Arc::new(self);

        // Build router with a body size limit to guard against oversized foreign payloads.
        // 4MB is generous for natural-language queries while blocking obvious DoS attempts.
        let app = create_router(app_state)
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024)) // 4MB
            .layer(TraceLayer::new_for_http());

        tracing::info!("Starting Shammah agent server on {}", addr);

        // Start server
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    /// Get reference to Claude client
    pub fn claude_client(&self) -> &Arc<ClaudeClient> {
        &self.claude_client
    }

    /// Resolve the cloud provider to use for a given request.
    ///
    /// If `name` matches a configured provider, returns that provider.
    /// If `name` is `None` or unrecognised, returns the first configured cloud provider
    /// (if any). Returns `None` when `providers` is empty (caller falls back to
    /// `claude_client`).
    pub fn provider_for_name(&self, name: Option<&str>) -> Option<&Arc<dyn LlmProvider>> {
        if self.providers.is_empty() {
            return None;
        }
        if let Some(n) = name {
            if let Some(p) = self
                .providers
                .iter()
                .find(|p| p.name().eq_ignore_ascii_case(n))
            {
                return Some(p);
            }
        }
        self.providers.first()
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

    /// Get reference to brain registry
    pub fn brain_registry(&self) -> &Arc<BrainRegistry> {
        &self.brain_registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_provider_for_name_unknown_falls_back_to_first() {
        let providers = make_providers(&["claude", "grok"]);
        // When name is unknown, provider_for_name returns providers.first()
        let matched = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("unknown"));
        let result = matched.or_else(|| providers.first());
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "claude");
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
