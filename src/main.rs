// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::Result;
use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;

use shammah::claude::ClaudeClient;
use shammah::cli::output_layer::OutputManagerLayer;
use shammah::cli::{ConversationHistory, Repl};
use shammah::config::{load_config, Config};
use shammah::crisis::CrisisDetector;
use shammah::metrics::MetricsLogger;
use shammah::models::ThresholdRouter;
use shammah::providers::create_provider;
use shammah::router::Router;
use tracing_subscriber::prelude::*;

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

    /// Use raw terminal mode instead of TUI (enables rustyline)
    #[arg(long = "raw", conflicts_with = "no_tui")]
    raw_mode: bool,

    /// Alias for --raw (for backwards compatibility)
    #[arg(long = "no-tui")]
    no_tui: bool,
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

/// Create a ClaudeClient with the configured provider
///
/// This function creates a provider based on the teacher configuration
/// and wraps it in a ClaudeClient for backwards compatibility.
fn create_claude_client_with_provider(config: &Config) -> Result<ClaudeClient> {
    let provider = create_provider(&config.teacher)?;
    Ok(ClaudeClient::with_provider(provider))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install panic handler to cleanup terminal on panic
    install_panic_handler();

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
        let claude_client = create_claude_client_with_provider(&config)?;
        let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

        // Create REPL (will detect non-interactive mode automatically)
        let mut repl = Repl::new(config, claude_client, router, metrics_logger).await;

        // Process the piped query and exit
        let response = repl.process_query(input.trim()).await?;
        println!("{}", response);

        return Ok(());
    }

    // Initialize tracing with custom OutputManager layer
    init_tracing();

    // Create global OutputManager and StatusBar (single instances for entire application)
    use shammah::cli::{OutputManager, StatusBar};
    use shammah::cli::global_output::{set_global_output, set_global_status};
    let output_manager = Arc::new(OutputManager::new());
    let status_bar = Arc::new(StatusBar::new());
    set_global_output(output_manager.clone());
    set_global_status(status_bar.clone());

    // Load configuration
    let mut config = load_config()?;

    // Override TUI setting if --raw or --no-tui flag is provided
    if args.raw_mode || args.no_tui {
        config.tui_enabled = false;
    }

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
                if std::env::var("SHAMMAH_DEBUG").is_ok() {
                    eprintln!(
                        "✓ Loaded threshold router with {} queries",
                        router.stats().total_queries
                    );
                }
                router
            }
            Err(e) => {
                if std::env::var("SHAMMAH_DEBUG").is_ok() {
                    eprintln!("Warning: Failed to load threshold router: {}", e);
                    eprintln!("  Creating new threshold router");
                }
                ThresholdRouter::new()
            }
        }
    } else {
        if std::env::var("SHAMMAH_DEBUG").is_ok() {
            eprintln!("Creating new threshold router");
        }
        ThresholdRouter::new()
    };

    // Create router with threshold router
    let router = Router::new(crisis_detector, threshold_router);

    // Create Claude client
    let claude_client = create_claude_client_with_provider(&config)?;

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
                    if std::env::var("SHAMMAH_DEBUG").is_ok() {
                        eprintln!("✓ Restored conversation from session");
                    }
                    std::fs::remove_file(&session_path)?;
                }
                Err(e) => {
                    if std::env::var("SHAMMAH_DEBUG").is_ok() {
                        eprintln!("⚠️  Failed to restore session: {}", e);
                    }
                }
            }
        }
    }

    // Run REPL (potentially with initial prompt)
    if std::env::var("SHAMMAH_DEBUG").is_ok() {
        eprintln!("[DEBUG] Starting REPL...");
    }

    // Use event loop mode by default (automatic detection)
    // Falls back to traditional mode if TUI is not available or --raw is used
    if std::env::var("SHAMMAH_DEBUG").is_ok() {
        eprintln!("[DEBUG] Starting REPL (event loop with fallback)...");
    }

    // Try event loop first (requires TUI)
    // If TUI is not available, run_event_loop() will automatically fall back
    repl.run_event_loop(args.initial_prompt).await?;

    if std::env::var("SHAMMAH_DEBUG").is_ok() {
        eprintln!("[DEBUG] REPL exited, returning from main");
    }
    Ok(())
}

/// Install panic handler to cleanup terminal state on panic
///
/// If the program panics while in raw mode (TUI active), the terminal
/// can be left in a broken state. This handler ensures proper cleanup.
fn install_panic_handler() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Emergency terminal cleanup
        use crossterm::{cursor, execute, terminal};
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            cursor::Show,
            terminal::Clear(terminal::ClearType::FromCursorDown)
        );

        // Call the default panic handler
        default_panic(info);
    }));
}

/// Initialize tracing with custom OutputManager layer
///
/// This routes all tracing logs (from dependencies and our code) through
/// the OutputManager so they appear in the TUI instead of printing directly.
fn init_tracing() {
    // Check if debug logging should be enabled
    let show_debug = std::env::var("SHAMMAH_DEBUG")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    // Create our custom output layer
    let output_layer = if show_debug {
        OutputManagerLayer::with_debug()
    } else {
        OutputManagerLayer::new()
    };

    // Create environment filter for log level control
    // Default: INFO level, can be overridden with RUST_LOG env var
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // Build the subscriber with our custom layer
    tracing_subscriber::registry()
        .with(env_filter)
        .with(output_layer)
        .init();

    // Bridge log crate → tracing (for dependencies using log crate)
    // Do this after subscriber is set up
    tracing_log::LogTracer::init().ok();
}

/// Run HTTP daemon server
async fn run_daemon(bind_address: String) -> Result<()> {
    use shammah::server::{AgentServer, ServerConfig};
    use shammah::models::{BootstrapLoader, GeneratorState, DevicePreference, TrainingCoordinator};
    use shammah::local::LocalGenerator;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use shammah::{output_progress, output_status};

    // Initialize tracing with custom OutputManager layer
    init_tracing();

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
    let claude_client = create_claude_client_with_provider(&config)?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Initialize BootstrapLoader for progressive Qwen model loading
    output_progress!("⏳ Initializing Qwen model (background)...");
    let generator_state = Arc::new(RwLock::new(GeneratorState::Initializing));
    let bootstrap_loader = Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None));

    // Start background model loading
    let loader_clone = Arc::clone(&bootstrap_loader);
    let state_clone = Arc::clone(&generator_state);
    tokio::spawn(async move {
        // TODO: Read from config instead of hardcoded defaults
        if let Err(e) = loader_clone
            .load_generator_async("Qwen2", "Medium", DevicePreference::Auto)
            .await
        {
            output_status!("⚠️  Model loading failed: {}", e);
            output_status!("   Will forward all queries to Claude");
            let mut state = state_clone.write().await;
            *state = GeneratorState::Failed {
                error: format!("{}", e),
            };
        }
    });

    // Create local generator (will receive model when ready)
    let local_generator = Arc::new(RwLock::new(LocalGenerator::new()));

    // Monitor generator state and inject model when ready
    let gen_clone = Arc::clone(&local_generator);
    let state_monitor = Arc::clone(&generator_state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let state = state_monitor.read().await;
            if let GeneratorState::Ready { model, .. } = &*state {
                // Inject Qwen model into LocalGenerator
                // Note: tokenizer is now embedded in GeneratorModel backend
                let mut gen = gen_clone.write().await;
                *gen = LocalGenerator::with_models(
                    Some(Arc::clone(model)),
                    None, // Tokenizer is embedded in GeneratorModel
                );

                output_status!("✓ Qwen model ready - local generation enabled");
                break; // Stop monitoring once injected
            } else if matches!(
                *state,
                GeneratorState::Failed { .. } | GeneratorState::NotAvailable
            ) {
                break; // Stop monitoring on failure
            }
        }
    });

    // Initialize LoRA fine-tuning system
    let training_coordinator = Arc::new(TrainingCoordinator::new(
        100,  // buffer_size: keep last 100 examples
        10,   // threshold: train after 10 examples
        true, // auto_train: enabled
    ));

    output_status!("✓ LoRA fine-tuning enabled (weighted training)");

    // Create server configuration
    let server_config = ServerConfig {
        bind_address: config.server.bind_address.clone(),
        max_sessions: config.server.max_sessions,
        session_timeout_minutes: config.server.session_timeout_minutes,
        auth_enabled: config.server.auth_enabled,
        api_keys: config.server.api_keys.clone(),
    };

    // Create and start agent server (with LocalGenerator support)
    let server = AgentServer::new(
        config,
        server_config,
        claude_client,
        router,
        metrics_logger,
        local_generator,
        bootstrap_loader,
        generator_state,
        training_coordinator,
    )?;
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
    let claude_client = create_claude_client_with_provider(&config)?;
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create REPL in non-interactive mode
    let mut repl = Repl::new(config, claude_client, router, metrics_logger).await;

    // Process query and print result
    let response = repl.process_query(query).await?;
    println!("{}", response);

    Ok(())
}
