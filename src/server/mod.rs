// Shammah - Agent Server Module
// HTTP daemon mode for multi-tenant agent serving

mod handlers;
mod middleware;
mod session;

pub use handlers::{create_router, health_check, metrics_endpoint};
pub use middleware::auth_middleware;
pub use session::{SessionManager, SessionState};

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;

use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::metrics::MetricsLogger;
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
            bind_address: "127.0.0.1:8000".to_string(),
            max_sessions: 100,
            session_timeout_minutes: 30,
            auth_enabled: false,
            api_keys: vec![],
        }
    }
}

/// Main agent server structure
pub struct AgentServer {
    /// Claude API client (shared across sessions)
    claude_client: Arc<ClaudeClient>,
    /// Router for decision-making (shared, read-write lock)
    router: Arc<RwLock<Router>>,
    /// Metrics logger (shared)
    metrics_logger: Arc<MetricsLogger>,
    /// Session manager
    session_manager: Arc<SessionManager>,
    /// Server configuration
    config: ServerConfig,
}

impl AgentServer {
    /// Create a new agent server
    pub fn new(
        config: Config,
        server_config: ServerConfig,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
    ) -> Result<Self> {
        let session_manager = SessionManager::new(
            server_config.max_sessions,
            server_config.session_timeout_minutes,
        );

        Ok(Self {
            claude_client: Arc::new(claude_client),
            router: Arc::new(RwLock::new(router)),
            metrics_logger: Arc::new(metrics_logger),
            session_manager: Arc::new(session_manager),
            config: server_config,
        })
    }

    /// Start the HTTP server
    pub async fn serve(self) -> Result<()> {
        let addr: SocketAddr = self.config.bind_address.parse()?;

        // Create application state
        let app_state = Arc::new(self);

        // Build router
        let app = create_router(app_state).layer(TraceLayer::new_for_http());

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

    /// Get server configuration
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }
}
