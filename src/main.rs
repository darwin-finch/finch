// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::Result;
use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use shammah::claude::ClaudeClient;
use shammah::cli::{ConversationHistory, Repl};
use shammah::config::load_config;
use shammah::crisis::CrisisDetector;
use shammah::metrics::MetricsLogger;
use shammah::models::ThresholdRouter;
use shammah::router::Router;

#[derive(Parser, Debug)]
#[command(name = "shammah")]
#[command(about = "Local-first Constitutional AI Proxy", version)]
struct Args {
    /// Run mode
    #[command(subcommand)]
    command: Option<Command>,

    /// Initial prompt to send after startup (REPL mode)
    #[arg(long = "initial-prompt")]
    initial_prompt: Option<String>,

    /// Path to session state file to restore (REPL mode)
    #[arg(long = "restore-session")]
    restore_session: Option<PathBuf>,
}

#[derive(Parser, Debug)]
enum Command {
    /// Run HTTP daemon server
    Daemon {
        /// Bind address (default: 127.0.0.1:8000)
        #[arg(long, default_value = "127.0.0.1:8000")]
        bind: String,
    },
    /// Execute a single query
    Query {
        /// Query text
        query: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command-line arguments
    let args = Args::parse();

    // Dispatch based on command
    match args.command {
        Some(Command::Daemon { bind }) => {
            return run_daemon(bind).await;
        }
        Some(Command::Query { query }) => {
            return run_query(&query).await;
        }
        None => {
            // Fall through to REPL mode (check for piped input first)
        }
    }

    // Check for piped input BEFORE initializing anything else
    if !io::stdin().is_terminal() {
        // Piped input mode: read query from stdin and process as single query
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;

        // Skip processing if input is empty
        if input.trim().is_empty() {
            return Ok(());
        }

        // Initialize minimal components for piped mode
        let config = load_config()?;
        let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

        let models_dir = dirs::home_dir()
            .map(|home| home.join(".shammah").join("models"))
            .expect("Failed to determine home directory");
        std::fs::create_dir_all(&models_dir)?;

        let threshold_router_path = models_dir.join("threshold_router.json");
        let threshold_router = if threshold_router_path.exists() {
            ThresholdRouter::load(&threshold_router_path).unwrap_or_else(|_| ThresholdRouter::new())
        } else {
            ThresholdRouter::new()
        };

        let router = Router::new(crisis_detector, threshold_router);
        let claude_client = ClaudeClient::new(config.api_key.clone())?;
        let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

        // Create REPL (will detect non-interactive mode automatically)
        let mut repl = Repl::new(config, claude_client, router, metrics_logger).await;

        // Process the piped query and exit
        let response = repl.process_query(input.trim()).await?;
        println!("{}", response);

        return Ok(());
    }

    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_config()?;

    // Load crisis detector
    let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

    // Load or create threshold router
    let models_dir = dirs::home_dir()
        .map(|home| home.join(".shammah").join("models"))
        .expect("Failed to determine home directory");
    std::fs::create_dir_all(&models_dir)?;

    let threshold_router_path = models_dir.join("threshold_router.json");
    let threshold_router = if threshold_router_path.exists() {
        match ThresholdRouter::load(&threshold_router_path) {
            Ok(router) => {
                eprintln!(
                    "✓ Loaded threshold router with {} queries",
                    router.stats().total_queries
                );
                router
            }
            Err(e) => {
                eprintln!("Warning: Failed to load threshold router: {}", e);
                eprintln!("  Creating new threshold router");
                ThresholdRouter::new()
            }
        }
    } else {
        eprintln!("Creating new threshold router");
        ThresholdRouter::new()
    };

    // Create router with threshold router
    let router = Router::new(crisis_detector, threshold_router);

    // Create Claude client
    let claude_client = ClaudeClient::new(config.api_key.clone())?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create and run REPL
    let mut repl = Repl::new(config, claude_client, router, metrics_logger).await;

    // Restore session if requested
    if let Some(session_path) = args.restore_session {
        if session_path.exists() {
            match ConversationHistory::load(&session_path) {
                Ok(history) => {
                    repl.restore_conversation(history);
                    eprintln!("✓ Restored conversation from session");
                    std::fs::remove_file(&session_path)?;
                }
                Err(e) => {
                    eprintln!("⚠️  Failed to restore session: {}", e);
                }
            }
        }
    }

    // Run REPL (potentially with initial prompt)
    repl.run_with_initial_prompt(args.initial_prompt).await?;

    Ok(())
}

/// Run HTTP daemon server
async fn run_daemon(bind_address: String) -> Result<()> {
    use shammah::server::{AgentServer, ServerConfig};

    // Initialize tracing
    tracing_subscriber::fmt::init();

    tracing::info!("Starting Shammah in daemon mode");

    // Load configuration
    let mut config = load_config()?;
    config.server.enabled = true;
    config.server.bind_address = bind_address;

    // Load crisis detector
    let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

    // Load or create threshold router
    let models_dir = dirs::home_dir()
        .map(|home| home.join(".shammah").join("models"))
        .expect("Failed to determine home directory");
    std::fs::create_dir_all(&models_dir)?;

    let threshold_router_path = models_dir.join("threshold_router.json");
    let threshold_router = if threshold_router_path.exists() {
        match ThresholdRouter::load(&threshold_router_path) {
            Ok(router) => {
                tracing::info!(
                    total_queries = router.stats().total_queries,
                    "Loaded threshold router"
                );
                router
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load threshold router, creating new one");
                ThresholdRouter::new()
            }
        }
    } else {
        tracing::info!("Creating new threshold router");
        ThresholdRouter::new()
    };

    // Create router
    let router = Router::new(crisis_detector, threshold_router);

    // Create Claude client
    let claude_client = ClaudeClient::new(config.api_key.clone())?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create server configuration
    let server_config = ServerConfig {
        bind_address: config.server.bind_address.clone(),
        max_sessions: config.server.max_sessions,
        session_timeout_minutes: config.server.session_timeout_minutes,
        auth_enabled: config.server.auth_enabled,
        api_keys: config.server.api_keys.clone(),
    };

    // Create and start agent server
    let server = AgentServer::new(config, server_config, claude_client, router, metrics_logger)?;
    server.serve().await?;

    Ok(())
}

/// Run a single query
async fn run_query(query: &str) -> Result<()> {
    // Initialize minimal components
    let config = load_config()?;
    let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

    let models_dir = dirs::home_dir()
        .map(|home| home.join(".shammah").join("models"))
        .expect("Failed to determine home directory");
    std::fs::create_dir_all(&models_dir)?;

    let threshold_router_path = models_dir.join("threshold_router.json");
    let threshold_router = if threshold_router_path.exists() {
        ThresholdRouter::load(&threshold_router_path).unwrap_or_else(|_| ThresholdRouter::new())
    } else {
        ThresholdRouter::new()
    };

    let router = Router::new(crisis_detector, threshold_router);
    let claude_client = ClaudeClient::new(config.api_key.clone())?;
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create REPL in non-interactive mode
    let mut repl = Repl::new(config, claude_client, router, metrics_logger).await;

    // Process query and print result
    let response = repl.process_query(query).await?;
    println!("{}", response);

    Ok(())
}
