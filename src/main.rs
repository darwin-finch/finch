// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;

use finch::claude::ClaudeClient;
use finch::cli::output_layer::OutputManagerLayer;
use finch::cli::{ConversationHistory, Repl};
use finch::config::{load_config, Config};
use finch::metrics::MetricsLogger;
use finch::models::ThresholdRouter;
use finch::providers::create_provider;
use finch::router::Router;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "finch")]
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

    /// Direct mode - talk directly to teacher API, bypass daemon
    #[arg(long = "direct")]
    direct: bool,
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

/// Build a teacher list from well-known environment variables and config files.
/// Collects ALL available keys so every provider the user has configured is available.
fn build_teachers_from_env() -> Vec<finch::config::TeacherEntry> {
    let mut teachers: Vec<finch::config::TeacherEntry> = Vec::new();
    let mut seen_providers = std::collections::HashSet::new();

    let mut add = |provider: &str, key: &str| {
        if seen_providers.contains(provider) { return; }
        seen_providers.insert(provider.to_string());
        teachers.push(finch::config::TeacherEntry {
            provider: provider.to_string(),
            api_key: key.trim().to_string(),
            model: None,
            base_url: None,
            name: None,
        });
    };

    // 1. Claude Code config file (~/.claude/settings.json)
    if let Some(home) = dirs::home_dir() {
        let claude_settings = home.join(".claude").join("settings.json");
        if let Ok(contents) = std::fs::read_to_string(&claude_settings) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(key) = json.get("apiKey").and_then(|v| v.as_str()) {
                    if !key.trim().is_empty() {
                        add("claude", key);
                    }
                }
            }
        }
    }

    // 2. Environment variables
    let candidates = [
        ("ANTHROPIC_API_KEY", "claude"),
        ("OPENAI_API_KEY",    "openai"),
        ("GROK_API_KEY",      "grok"),
        ("XAI_API_KEY",       "grok"),
        ("GEMINI_API_KEY",    "gemini"),
        ("MISTRAL_API_KEY",   "mistral"),
        ("GROQ_API_KEY",      "groq"),
    ];

    for (env_var, provider) in &candidates {
        if let Ok(key) = std::env::var(env_var) {
            if !key.trim().is_empty() {
                add(provider, &key);
            }
        }
    }

    teachers
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
    use finch::cli::{OutputManager, StatusBar};
    use finch::cli::global_output::{set_global_output, set_global_status};
    use finch::config::ColorScheme;

    let output_manager = Arc::new(OutputManager::new(ColorScheme::default()));
    let status_bar = Arc::new(StatusBar::new());

    // Disable stdout immediately for TUI mode (will re-enable for --raw/--no-tui later)
    output_manager.disable_stdout();

    // Set as global BEFORE init_tracing() to prevent lazy initialization
    set_global_output(output_manager.clone());
    set_global_status(status_bar.clone());

    // Check if debug logging is enabled in config (before init_tracing)
    // This allows the debug_logging feature flag to control log verbosity
    if let Ok(temp_config) = load_config() {
        if temp_config.features.debug_logging {
            // Set RUST_LOG to debug if not already set by user
            if std::env::var("RUST_LOG").is_err() {
                std::env::set_var("RUST_LOG", "debug");
            }
        }
    }

    // NOW initialize tracing (will use the global OutputManager we just configured)
    init_tracing();

    // Load configuration (or run setup if missing)
    let mut config = match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}", e);

            // Before showing the wizard, try to auto-detect API keys.
            // If any exist (env vars, Claude Code config, etc.) just start immediately.
            let auto_teachers = build_teachers_from_env();
            if !auto_teachers.is_empty() {
                let names: Vec<&str> = auto_teachers.iter().map(|t| t.provider.as_str()).collect();
                eprintln!("\n\x1b[1;32m✓ Auto-configured: {}\x1b[0m", names.join(", "));
                eprintln!("\x1b[33m  Run `finch setup` any time to change settings.\x1b[0m\n");
                let cfg = Config::new(auto_teachers);
                cfg.save().ok();
                cfg
            } else {

            eprintln!("\n\x1b[1;33m⚠️  Running first-time setup wizard...\x1b[0m\n");

            // Run setup wizard
            use finch::cli::show_setup_wizard;
            match show_setup_wizard() {
                Ok(result) => {
                    // Create and save config from wizard results
                    let backend_device = result.backend_device();
                    let backend_enabled = result.backend_enabled;
                    let inference_provider = result.inference_provider;
                    let model_family = result.model_family;
                    let model_size = result.model_size;
                    let custom_model_repo = result.custom_model_repo;
                    let active_theme = result.active_theme.clone();
                    let default_persona = result.default_persona.clone();
                    let daemon_only_mode = result.daemon_only_mode;
                    let mdns_discovery = result.mdns_discovery;

                    // Patch any empty API keys with auto-detected values
                    let mut teachers = result.teachers;
                    let auto = build_teachers_from_env();
                    for teacher in &mut teachers {
                        if teacher.api_key.is_empty() {
                            if let Some(detected) = auto.iter().find(|t| t.provider == teacher.provider) {
                                teacher.api_key = detected.api_key.clone();
                            } else if let Some(first) = auto.first() {
                                // If provider doesn't match, use whatever key we found
                                teacher.api_key = first.api_key.clone();
                                teacher.provider = first.provider.clone();
                            }
                        }
                    }
                    // If still no teachers with keys, add auto-detected ones
                    if teachers.iter().all(|t| t.api_key.is_empty()) {
                        teachers = auto;
                    }

                    let mut new_config = Config::new(teachers);
                    new_config.backend = finch::config::BackendConfig {
                        enabled: backend_enabled,
                        inference_provider,
                        execution_target: backend_device,
                        model_family,
                        model_size,
                        model_repo: custom_model_repo,
                        ..Default::default()
                    };
                    new_config.active_theme = active_theme;
                    new_config.active_persona = default_persona;
                    if let Some(hf_tok) = result.hf_token {
                        if !hf_tok.is_empty() {
                            new_config.huggingface_token = Some(hf_tok);
                        }
                    }
                    new_config.features = finch::config::FeaturesConfig {
                        auto_approve_tools: result.auto_approve_tools,
                        streaming_enabled: result.streaming_enabled,
                        debug_logging: result.debug_logging,
                        #[cfg(target_os = "macos")]
                        gui_automation: result.gui_automation,
                    };
                    if daemon_only_mode {
                        new_config.server.mode = "daemon-only".to_string();
                    }
                    if mdns_discovery {
                        new_config.server.advertise = true;
                        new_config.client.auto_discover = true;
                    }
                    new_config.streaming_enabled = new_config.features.streaming_enabled;
                    new_config.save()?;
                    eprintln!("\n\x1b[1;32m✓ Configuration saved!\x1b[0m\n");
                    new_config
                }
                Err(wizard_err) if wizard_err.to_string().contains("Setup cancelled") => {
                    // User pressed Escape/Ctrl+C — don't crash, fall back gracefully
                    eprintln!("\n\x1b[33mSetup skipped. Detecting API keys from environment...\x1b[0m");

                    let teachers = build_teachers_from_env();

                    if teachers.is_empty() {
                        eprintln!(
                            "\x1b[33mNo API keys found. Set ANTHROPIC_API_KEY (or OPENAI_API_KEY / GROK_API_KEY)\x1b[0m"
                        );
                        eprintln!("\x1b[33mand re-run, or run `finch setup` to configure interactively.\x1b[0m\n");
                    } else {
                        let names: Vec<&str> = teachers.iter().map(|t| t.provider.as_str()).collect();
                        eprintln!("\x1b[32m✓ Auto-configured: {}\x1b[0m\n", names.join(", "));
                    }

                    let cfg = Config::new(teachers);
                    // Save so next launch doesn't show the wizard again
                    if cfg.save().is_err() {
                        // Non-fatal — we'll just show the wizard again next time
                    }
                    cfg
                }
                Err(e) => return Err(e),
            }
            } // end else (no auto-detected keys)
        }
    };

    // Override TUI setting if --raw or --no-tui flag is provided
    if args.raw_mode || args.no_tui {
        config.tui_enabled = false;
        // Re-enable stdout for non-TUI modes
        output_manager.enable_stdout();
    }

    // Check for --direct flag (bypass daemon)
    // In direct mode: no daemon connection, talk directly to teacher API
    let use_daemon = !args.direct;

    // Load or create threshold router
    let models_dir = dirs::home_dir()
        .map(|home| home.join(".finch").join("models"))
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

    // Create router
    let router = Router::new(threshold_router);

    // Create Claude client
    let claude_client = create_claude_client_with_provider(&config)?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Try to connect to daemon BEFORE creating Repl
    // This allows Repl to suppress local model logs if daemon is available
    use finch::client::{DaemonClient, DaemonConfig};
    let daemon_client = if use_daemon && config.client.use_daemon {
        let daemon_config = DaemonConfig {
            bind_address: config.client.daemon_address.clone(),
            auto_spawn: config.client.auto_spawn,
            timeout_seconds: 5,
        };
        match DaemonClient::connect(daemon_config).await {
            Ok(client) => {
                output_manager.write_status("✓ Connected to daemon");
                Some(Arc::new(client))
            }
            Err(e) => {
                if std::env::var("SHAMMAH_DEBUG").is_ok() {
                    eprintln!("Failed to connect to daemon: {}", e);
                }
                None
            }
        }
    } else {
        if args.direct && io::stdout().is_terminal() {
            output_manager.write_status("⚠️  Direct mode - bypassing daemon, using teacher API");
        }
        None
    };

    // Create and run REPL (with full TUI support)
    // Pass daemon_client so Repl knows whether to suppress local model logs
    let mut repl = Repl::new(config, claude_client, router, metrics_logger, daemon_client).await;

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
    // Note: config.features.debug_logging sets RUST_LOG=debug before init_tracing()
    // Users can also manually set RUST_LOG for custom log levels
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
    use finch::daemon::{DaemonLifecycle, ensure_daemon_running};

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
    println!("Logs: ~/.finch/daemon.log");

    // Use ensure_daemon_running to spawn and wait for health check
    ensure_daemon_running(Some(&bind_address)).await?;

    // Get PID for display
    let pid = lifecycle.read_pid()?;
    println!("✓ Daemon started successfully (PID: {})", pid);

    Ok(())
}

/// Stop the running daemon
fn run_daemon_stop() -> Result<()> {
    use finch::daemon::DaemonLifecycle;

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
    use finch::daemon::DaemonLifecycle;

    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is running
    if !lifecycle.is_running() {
        println!("\x1b[1;33m⚠ Daemon is not running\x1b[0m");
        println!("\nStart the daemon with:");
        println!("  \x1b[1;36mfinch daemon-start\x1b[0m");
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
    let venv_dir = home.join(".finch/venv");
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
    println!("\n2️⃣  Creating virtual environment at ~/.finch/venv...");

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
    println!("  \x1b[1;36m~/.finch/venv/bin/python scripts/train_lora.py\x1b[0m");
    println!("\nTraining will run automatically when you provide feedback.");

    Ok(())
}

async fn run_daemon(bind_address: String) -> Result<()> {
    use finch::server::{AgentServer, ServerConfig};
    use finch::models::{BootstrapLoader, GeneratorState, DevicePreference, TrainingCoordinator};
    use finch::local::LocalGenerator;
    use finch::daemon::DaemonLifecycle;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use finch::{output_progress, output_status};

    // Check if debug logging is enabled in config (before setting up tracing)
    // This allows the debug_logging feature flag to control log verbosity
    if let Ok(temp_config) = load_config() {
        if temp_config.features.debug_logging {
            // Set RUST_LOG to debug if not already set by user
            if std::env::var("RUST_LOG").is_err() {
                std::env::set_var("RUST_LOG", "debug");
            }
        }
    }

    // Set up file logging for daemon (append to ~/.finch/daemon.log)
    let log_path = dirs::home_dir()
        .context("Failed to determine home directory")?
        .join(".finch")
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
        anyhow::bail!(finch::errors::daemon_already_running_error(existing_pid));
    }

    // Write PID file
    lifecycle.write_pid()?;
    tracing::info!(pid = std::process::id(), "Daemon PID file written");

    // Load configuration
    let mut config = load_config()?;
    config.server.enabled = true;
    config.server.bind_address = bind_address.clone();

    // Load or create threshold router
    let models_dir = dirs::home_dir()
        .map(|home| home.join(".finch").join("models"))
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
    let router = Router::new(threshold_router);

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
        let provider = config.backend.inference_provider;
        let model_family = config.backend.model_family;
        let model_size = config.backend.model_size;
        let device = config.backend.execution_target;
        let model_repo = config.backend.model_repo.clone();
        tokio::spawn(async move {
            if let Err(e) = loader_clone
                .load_generator_async(provider, model_family, model_size, device, model_repo)
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
        config.clone(),
        server_config.clone(),
        claude_client,
        router,
        metrics_logger,
        local_generator,
        bootstrap_loader,
        generator_state,
        training_coordinator,
    )?;

    // Set up mDNS service advertisement if enabled
    let service_discovery = if config.server.advertise {
        use finch::service::{ServiceDiscovery, ServiceConfig};

        let service_config = ServiceConfig {
            name: config.server.service_name.clone(),
            description: config.server.service_description.clone(),
            model: format!("{:?}", config.backend.model_size),  // e.g., "Small", "Medium", "Large"
            capabilities: vec!["code".to_string(), "general".to_string(), "tool-use".to_string()],
        };

        match ServiceDiscovery::new(service_config) {
            Ok(discovery) => {
                // Extract port from bind_address
                let port = config.server.bind_address
                    .split(':')
                    .last()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(11435);

                match discovery.advertise(port) {
                    Ok(_) => {
                        tracing::info!("✓ mDNS advertisement enabled");
                        Some(discovery)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to advertise service: {}. Continuing without mDNS.", e);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to create service discovery: {}. Continuing without mDNS.", e);
                None
            }
        }
    } else {
        None
    };

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

    // Stop mDNS advertisement if enabled
    if let Some(discovery) = service_discovery {
        if let Err(e) = discovery.stop() {
            tracing::warn!("Failed to stop service advertisement: {}", e);
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
    use finch::client::DaemonClient;
    use finch::daemon::ensure_daemon_running;

    // Load configuration
    let config = load_config()?;

    // Ensure daemon is running (auto-spawn if needed)
    if let Err(e) = ensure_daemon_running(Some(&config.client.daemon_address)).await {
        eprintln!("⚠️  Daemon failed to start: {}", e);
        eprintln!("   Using teacher API directly (no local model)");
        return run_query_teacher_only(query, &config).await;
    }

    // Create daemon client
    let daemon_config = finch::client::DaemonConfig::from_client_config(&config.client);
    let client = DaemonClient::connect(daemon_config).await?;

    // Send query to daemon
    let response = client.query_text(query).await?;
    println!("{}", response);

    Ok(())
}

/// Run query using teacher API only (fallback when daemon fails)
async fn run_query_teacher_only(query: &str, config: &Config) -> Result<()> {
    use finch::claude::{MessageRequest, ContentBlock};

    eprintln!("⚠️  Running in teacher-only mode (no local model)");

    // Create teacher client
    let claude_client = create_claude_client_with_provider(config)?;

    // Create simple request
    let request = MessageRequest {
        model: config.active_teacher()
            .and_then(|t| t.model.clone())
            .unwrap_or_else(|| "claude-sonnet-4-5-20250929".to_string()),
        max_tokens: 8000,
        messages: vec![finch::claude::Message {
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
    use finch::cli::show_setup_wizard;
    use finch::config::{BackendConfig, Config};

    println!("Starting Shammah setup wizard...\n");

    // Run the wizard
    let result = show_setup_wizard()?;

    // Extract values before partial move
    let backend_device = result.backend_device();
    let backend_enabled = result.backend_enabled;
    let inference_provider = result.inference_provider;
    let model_family = result.model_family;
    let model_size = result.model_size;
    let custom_model_repo = result.custom_model_repo;

    // Create config from wizard results
    let mut config = Config::new(result.teachers);

    // Update backend config with selected provider, device, model family, and size
    config.backend = BackendConfig {
        enabled: backend_enabled,
        inference_provider,
        execution_target: backend_device,
        model_family,
        model_size,
        model_repo: custom_model_repo,
        ..Default::default()
    };

    // Update feature flags
    config.features = finch::config::FeaturesConfig {
        auto_approve_tools: result.auto_approve_tools,
        streaming_enabled: result.streaming_enabled,
        debug_logging: result.debug_logging,
        #[cfg(target_os = "macos")]
        gui_automation: false, // Not yet implemented in wizard
    };
    // Update deprecated streaming_enabled field for backward compat
    config.streaming_enabled = config.features.streaming_enabled;

    // Save configuration
    config.save()?;

    println!("\n✓ Configuration saved to ~/.finch/config.toml");
    println!("  You can now run: finch");
    println!("  Or start the daemon: finch daemon\n");

    Ok(())
}
