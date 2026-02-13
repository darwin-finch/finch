// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::{Context, Result};
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

    /// Force direct mode (bypass daemon, for debugging)
    #[arg(long = "no-daemon")]
    no_daemon: bool,
}

#[derive(Parser, Debug)]
enum Command {
    /// Run interactive setup wizard
    Setup,
    /// Run HTTP daemon server
    Daemon {
        /// Bind address (default: 127.0.0.1:8000)
        #[arg(long, default_value = "127.0.0.1:8000")]
        bind: String,
    },
    /// Start the daemon in background
    DaemonStart {
        /// Bind address (default: 127.0.0.1:11435)
        #[arg(long, default_value = "127.0.0.1:11435")]
        bind: String,
    },
    /// Stop the running daemon
    DaemonStop,
    /// Show daemon status
    DaemonStatus,
    /// Training commands
    Train {
        #[command(subcommand)]
        train_command: TrainCommand,
    },
    /// Execute a single query
    Query {
        /// Query text
        query: String,
    },
}

#[derive(Parser, Debug)]
enum TrainCommand {
    /// Install Python dependencies for LoRA training
    Setup,
}

/// Create a ClaudeClient with the configured provider
///
/// This function creates a provider based on the teacher configuration
/// and wraps it in a ClaudeClient for backwards compatibility.
fn create_claude_client_with_provider(config: &Config) -> Result<ClaudeClient> {
    let provider = create_provider(&config.teachers)?;
    Ok(ClaudeClient::with_provider(provider))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Suppress ONNX Runtime verbose logs BEFORE any initialization
    // Must be set early, before any ONNX library code runs
    // ORT_LOGGING_LEVEL: 0=Verbose, 1=Info, 2=Warning, 3=Error, 4=Fatal
    std::env::set_var("ORT_LOGGING_LEVEL", "3");  // Error and Fatal only

    // Install panic handler to cleanup terminal on panic
    install_panic_handler();

    // Parse command-line arguments
    let args = Args::parse();

    // Dispatch based on command
    match args.command {
        Some(Command::Setup) => {
            return run_setup().await;
        }
        Some(Command::Daemon { bind }) => {
            return run_daemon(bind).await;
        }
        Some(Command::DaemonStart { bind }) => {
            return run_daemon_start(bind).await;
        }
        Some(Command::DaemonStop) => {
            return run_daemon_stop();
        }
        Some(Command::DaemonStatus) => {
            return run_daemon_status().await;
        }
        Some(Command::Train { train_command }) => {
            return run_train_command(train_command).await;
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

        // Run query via daemon
        return run_query(input.trim()).await;
    }

    // CRITICAL: Create and configure OutputManager BEFORE initializing tracing
    // This prevents lazy initialization with stdout enabled
    use shammah::cli::{OutputManager, StatusBar};
    use shammah::cli::global_output::{set_global_output, set_global_status};
    use shammah::config::ColorScheme;

    let output_manager = Arc::new(OutputManager::new(ColorScheme::default()));
    let status_bar = Arc::new(StatusBar::new());

    // Disable stdout immediately for TUI mode (will re-enable for --raw/--no-tui later)
    output_manager.disable_stdout();

    // Set as global BEFORE init_tracing() to prevent lazy initialization
    set_global_output(output_manager.clone());
    set_global_status(status_bar.clone());

    // NOW initialize tracing (will use the global OutputManager we just configured)
    init_tracing();

    // Load configuration (or run setup if missing)
    let mut config = match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}", e);
            eprintln!("\n\x1b[1;33m⚠️  Running first-time setup wizard...\x1b[0m\n");

            // Run setup wizard
            use shammah::cli::show_setup_wizard;
            let result = show_setup_wizard()?;

            // Create and save config
            let mut new_config = Config::new(result.teachers);
            new_config.backend = shammah::config::BackendConfig {
                enabled: result.backend_enabled,
                device: result.backend_device,
                model_family: result.model_family,
                model_size: result.model_size,
                model_repo: result.custom_model_repo,
                ..Default::default()
            };
            new_config.save()?;

            eprintln!("\n\x1b[1;32m✓ Configuration saved!\x1b[0m\n");
            new_config
        }
    };

    // Override TUI setting if --raw or --no-tui flag is provided
    if args.raw_mode || args.no_tui {
        config.tui_enabled = false;
        // Re-enable stdout for non-TUI modes
        output_manager.enable_stdout();
    }

    // Check for --no-daemon debug flag
    if args.no_daemon {
        eprintln!("⚠️  Running in no-daemon mode (debug only)");
        eprintln!("   Connecting directly to teacher API");

        // Read query if provided
        if let Some(query) = args.initial_prompt {
            return run_query_teacher_only(&query, &config).await;
        }

        eprintln!("Error: --no-daemon requires --initial-prompt");
        anyhow::bail!("--no-daemon mode requires --initial-prompt");
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

    // Create and run REPL (with full TUI support)
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

    // Run REPL (with full TUI event loop)
    if std::env::var("SHAMMAH_DEBUG").is_ok() {
        eprintln!("[DEBUG] Starting REPL with full TUI...");
    }

    // Use event loop mode (has all TUI features)
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
/// Start the daemon in background
async fn run_daemon_start(bind_address: String) -> Result<()> {
    use shammah::daemon::{DaemonLifecycle, ensure_daemon_running};

    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is already running
    if lifecycle.is_running() {
        let pid = lifecycle.read_pid()?;
        println!("Daemon is already running (PID: {})", pid);
        println!("Bind address: {}", bind_address);
        return Ok(());
    }

    println!("Starting daemon...");
    println!("Bind address: {}", bind_address);
    println!("Logs: ~/.shammah/daemon.log");

    // Use ensure_daemon_running to spawn and wait for health check
    ensure_daemon_running(Some(&bind_address)).await?;

    // Get PID for display
    let pid = lifecycle.read_pid()?;
    println!("✓ Daemon started successfully (PID: {})", pid);

    Ok(())
}

/// Stop the running daemon
fn run_daemon_stop() -> Result<()> {
    use shammah::daemon::DaemonLifecycle;

    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is running
    if !lifecycle.is_running() {
        println!("Daemon is not running");
        return Ok(());
    }

    // Get PID for display
    let pid = lifecycle.read_pid()?;
    println!("Stopping daemon (PID: {})...", pid);

    // Stop daemon
    lifecycle.stop_daemon()?;

    println!("✓ Daemon stopped successfully");
    Ok(())
}

/// Show daemon status
async fn run_daemon_status() -> Result<()> {
    use shammah::daemon::DaemonLifecycle;

    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is running
    if !lifecycle.is_running() {
        println!("\x1b[1;33m⚠ Daemon is not running\x1b[0m");
        println!("\nStart the daemon with:");
        println!("  \x1b[1;36mshammah daemon-start\x1b[0m");
        return Ok(());
    }

    // Get PID
    let pid = lifecycle.read_pid()?;

    // Query health endpoint
    let client = reqwest::Client::new();
    let daemon_url = format!("http://127.0.0.1:11435/health");

    let response = client
        .get(&daemon_url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if !response.status().is_success() {
        anyhow::bail!("Daemon returned error status: {}", response.status());
    }

    // Parse JSON response
    #[derive(serde::Deserialize)]
    struct HealthStatus {
        status: String,
        uptime_seconds: u64,
        active_sessions: usize,
    }

    let health: HealthStatus = response
        .json()
        .await
        .context("Failed to parse health response")?;

    // Display status
    println!("\x1b[1;32m✓ Daemon Status\x1b[0m");
    println!();
    println!("  Status:          \x1b[1;32m{}\x1b[0m", health.status);
    println!("  PID:             {}", pid);
    println!("  Uptime:          {}s", health.uptime_seconds);
    println!("  Active Sessions: {}", health.active_sessions);
    println!("  Bind Address:    127.0.0.1:11435");
    println!();

    Ok(())
}

/// Handle train subcommands
async fn run_train_command(train_command: TrainCommand) -> Result<()> {
    match train_command {
        TrainCommand::Setup => run_train_setup().await,
    }
}

/// Set up Python environment for LoRA training
async fn run_train_setup() -> Result<()> {
    use std::path::PathBuf;
    use std::process::Command;

    println!("\x1b[1;36m🔧 Setting up Python environment for LoRA training\x1b[0m\n");

    // Determine paths
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let venv_dir = home.join(".shammah/venv");
    let requirements_path = std::env::current_dir()?.join("scripts/requirements.txt");

    // Check if requirements.txt exists
    if !requirements_path.exists() {
        anyhow::bail!(
            "Requirements file not found at: {}\n\
             Make sure you're running from the project root directory.",
            requirements_path.display()
        );
    }

    // Step 1: Check Python version
    println!("1️⃣  Checking Python installation...");
    let python_check = Command::new("python3")
        .arg("--version")
        .output()
        .context("Failed to run 'python3 --version'. Is Python 3 installed?")?;

    if !python_check.status.success() {
        anyhow::bail!("Python 3 not found. Please install Python 3.8 or later.");
    }

    let python_version = String::from_utf8_lossy(&python_check.stdout);
    println!("   ✓ Found {}", python_version.trim());

    // Step 2: Create virtual environment
    println!("\n2️⃣  Creating virtual environment at ~/.shammah/venv...");

    if venv_dir.exists() {
        println!("   ⚠️  Virtual environment already exists, skipping creation");
    } else {
        let venv_status = Command::new("python3")
            .arg("-m")
            .arg("venv")
            .arg(&venv_dir)
            .status()
            .context("Failed to create virtual environment")?;

        if !venv_status.success() {
            anyhow::bail!("Failed to create virtual environment");
        }
        println!("   ✓ Virtual environment created");
    }

    // Step 3: Install dependencies
    println!("\n3️⃣  Installing Python dependencies...");
    println!("   (This may take several minutes)\n");

    let pip_path = if cfg!(target_os = "windows") {
        venv_dir.join("Scripts/pip.exe")
    } else {
        venv_dir.join("bin/pip")
    };

    let install_status = Command::new(&pip_path)
        .arg("install")
        .arg("-r")
        .arg(&requirements_path)
        .status()
        .context("Failed to run pip install")?;

    if !install_status.success() {
        anyhow::bail!("Failed to install Python dependencies");
    }

    println!("\n   ✓ Dependencies installed successfully");

    // Step 4: Verify installation
    println!("\n4️⃣  Verifying installation...");

    let python_path = if cfg!(target_os = "windows") {
        venv_dir.join("Scripts/python.exe")
    } else {
        venv_dir.join("bin/python")
    };

    let verify_status = Command::new(&python_path)
        .arg("-c")
        .arg("import torch, transformers, peft; print('✓ All packages imported successfully')")
        .status()
        .context("Failed to verify installation")?;

    if !verify_status.success() {
        anyhow::bail!("Package verification failed");
    }

    // Success message
    println!("\n\x1b[1;32m✅ Setup complete!\x1b[0m\n");
    println!("Python environment ready at: \x1b[1m{}\x1b[0m", venv_dir.display());
    println!("\nTo use the training scripts:");
    println!("  \x1b[1;36m~/.shammah/venv/bin/python scripts/train_lora.py\x1b[0m");
    println!("\nTraining will run automatically when you provide feedback.");

    Ok(())
}

async fn run_daemon(bind_address: String) -> Result<()> {
    use shammah::server::{AgentServer, ServerConfig};
    use shammah::models::{BootstrapLoader, GeneratorState, DevicePreference, TrainingCoordinator};
    use shammah::local::LocalGenerator;
    use shammah::daemon::DaemonLifecycle;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use shammah::{output_progress, output_status};

    // Set up file logging for daemon (append to ~/.shammah/daemon.log)
    let log_path = dirs::home_dir()
        .context("Failed to determine home directory")?
        .join(".shammah")
        .join("daemon.log");

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open daemon log: {}", log_path.display()))?;

    // Create a file logger layer
    use tracing_subscriber::fmt::writer::MakeWriter;
    let file_writer = Arc::new(log_file);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || file_writer.clone())
        .with_ansi(false);  // No ANSI colors in log file

    // Add file layer to tracing
    use tracing_subscriber::prelude::*;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .init();

    eprintln!("Daemon logs: {}", log_path.display());

    // Suppress ONNX Runtime verbose logs (must be set before library initialization)
    // ORT_LOGGING_LEVEL: 0=Verbose, 1=Info, 2=Warning, 3=Error, 4=Fatal
    std::env::set_var("ORT_LOGGING_LEVEL", "3");  // Error and Fatal only

    // Note: init_tracing() is NOT called in daemon mode - we set up file logging above instead

    tracing::info!("Starting Shammah in daemon mode");

    // Initialize daemon lifecycle (PID file management)
    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is already running
    if lifecycle.is_running() {
        let existing_pid = lifecycle.read_pid()?;
        anyhow::bail!(shammah::errors::daemon_already_running_error(existing_pid));
    }

    // Write PID file
    lifecycle.write_pid()?;
    tracing::info!(pid = std::process::id(), "Daemon PID file written");

    // Load configuration
    let mut config = load_config()?;
    config.server.enabled = true;
    config.server.bind_address = bind_address.clone();

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

    // Start background model loading (unless backend is disabled for proxy-only mode)
    if config.backend.enabled {
        let loader_clone = Arc::clone(&bootstrap_loader);
        let state_clone = Arc::clone(&generator_state);
        let model_family = config.backend.model_family;
        let model_size = config.backend.model_size;
        let device = config.backend.device;
        let model_repo = config.backend.model_repo.clone();
        tokio::spawn(async move {
            if let Err(e) = loader_clone
                .load_generator_async(model_family, model_size, device, model_repo)
                .await
            {
                output_status!("⚠️  Model loading failed: {}", e);
                output_status!("   Will forward all queries to teacher APIs");
                let mut state = state_clone.write().await;
                *state = GeneratorState::Failed {
                    error: format!("{}", e),
                };
            }
        });
    } else {
        // Proxy-only mode: Skip model loading
        output_status!("🔌 Proxy-only mode enabled (no local model)");
        output_status!("   All queries will be forwarded to teacher APIs");
        let mut state = generator_state.write().await;
        *state = GeneratorState::NotAvailable;
    }

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
                    Some(Arc::clone(model)), // Tokenizer is embedded in GeneratorModel
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

    // Set up graceful shutdown handling
    let server_handle = tokio::spawn(async move {
        server.serve().await
    });

    // Wait for shutdown signal (Ctrl+C or SIGTERM)
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT, shutting down gracefully");
        }
        result = server_handle => {
            match result {
                Ok(Ok(())) => {
                    tracing::info!("Server exited normally");
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "Server exited with error");
                }
                Err(e) => {
                    tracing::error!(error = %e, "Server task panicked");
                }
            }
        }
    }

    // Cleanup PID file on exit
    lifecycle.cleanup()?;
    tracing::info!("Daemon shutdown complete");

    Ok(())
}

/// Run a single query
/// Run a single query (daemon-only mode)
async fn run_query(query: &str) -> Result<()> {
    use shammah::client::DaemonClient;
    use shammah::daemon::ensure_daemon_running;

    // Load configuration
    let config = load_config()?;

    // Ensure daemon is running (auto-spawn if needed)
    if let Err(e) = ensure_daemon_running(Some(&config.client.daemon_address)).await {
        eprintln!("⚠️  Daemon failed to start: {}", e);
        eprintln!("   Using teacher API directly (no local model)");
        return run_query_teacher_only(query, &config).await;
    }

    // Create daemon client
    let daemon_config = shammah::client::DaemonConfig::from_client_config(&config.client);
    let client = DaemonClient::connect(daemon_config).await?;

    // Send query to daemon
    let response = client.query_text(query).await?;
    println!("{}", response);

    Ok(())
}

/// Run query using teacher API only (fallback when daemon fails)
async fn run_query_teacher_only(query: &str, config: &Config) -> Result<()> {
    use shammah::claude::{MessageRequest, ContentBlock};

    eprintln!("⚠️  Running in teacher-only mode (no local model)");

    // Create teacher client
    let claude_client = create_claude_client_with_provider(config)?;

    // Create simple request
    let request = MessageRequest {
        model: config.active_teacher()
            .and_then(|t| t.model.clone())
            .unwrap_or_else(|| "claude-sonnet-4-5-20250929".to_string()),
        max_tokens: 8000,
        messages: vec![shammah::claude::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: query.to_string(),
            }],
        }],
        tools: None,
    };

    // Send to teacher API
    let response = claude_client.send_message(&request).await?;

    // Extract text from response
    let text = response.content
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    println!("{}", text);

    Ok(())
}

/// Run interactive setup wizard
async fn run_setup() -> Result<()> {
    use shammah::cli::show_setup_wizard;
    use shammah::config::{BackendConfig, Config};

    println!("Starting Shammah setup wizard...\n");

    // Run the wizard
    let result = show_setup_wizard()?;

    // Create config from wizard results
    let mut config = Config::new(result.teachers);

    // Update backend config with selected device, model family, and size
    config.backend = BackendConfig {
        enabled: result.backend_enabled,
        device: result.backend_device,
        model_family: result.model_family,
        model_size: result.model_size,
        model_repo: result.custom_model_repo,
        ..Default::default()
    };

    // Save configuration
    config.save()?;

    println!("\n✓ Configuration saved to ~/.shammah/config.toml");
    println!("  You can now run: shammah");
    println!("  Or start the daemon: shammah daemon\n");

    Ok(())
}
