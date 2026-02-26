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

    /// Cloud-only mode - skip local model entirely, use teacher API directly.
    /// No model download, no daemon. Great for machines without much RAM,
    /// or when you only have a cloud API key (e.g. Grok via X Premium+).
    #[arg(long = "cloud-only", alias = "teacher-only")]
    cloud_only: bool,
}

#[derive(Parser, Debug)]
enum Command {
    /// Run interactive setup wizard
    Setup,
    /// Run HTTP daemon server
    Daemon {
        /// Bind address (default: 127.0.0.1:8000)
        // constant: crate::config::constants::DEFAULT_HTTP_ADDR
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
    /// Run as a network worker node (accepts queries from other machines)
    ///
    /// Binds to 0.0.0.0 by default so other machines on the network can
    /// delegate work to this node. Shows node identity and capabilities.
    Worker {
        /// Bind address (default: 0.0.0.0:8000 — accepts external connections)
        // constant: crate::config::constants::DEFAULT_WORKER_ADDR
        #[arg(long, default_value = "0.0.0.0:8000")]
        bind: String,
        /// Show node info and exit without starting server
        #[arg(long)]
        info: bool,
    },
    /// Show this node's identity and capabilities
    NodeInfo,
    /// Lotus Network device registration and account linking
    Network {
        #[command(subcommand)]
        network_command: NetworkCommand,
    },
    /// Manage Finch commercial license key
    License {
        #[command(subcommand)]
        license_command: Option<LicenseCommand>,
    },
    /// Run as an autonomous agent, working through a task backlog
    Agent {
        /// Persona name (builtin or ~/.finch/personas/<name>.toml) or path to .toml
        #[arg(long, default_value = "autonomous")]
        persona: String,

        /// Path to tasks.toml (default: ~/.finch/tasks.toml)
        #[arg(long)]
        tasks: Option<PathBuf>,

        /// Number of completed tasks between self-reflections (0 = disable)
        #[arg(long, default_value = "5")]
        reflect_every: usize,

        /// Complete one task then exit (for testing)
        #[arg(long)]
        once: bool,
    },
}

#[derive(Parser, Debug)]
enum NetworkCommand {
    /// Show this device's Lotus Network status
    Status,
    /// Register this device with the Lotus Network (no account required)
    Register,
    /// Link this device to a Lotus account using an invite code
    Join {
        /// Invite code from your Lotus account settings
        invite_code: String,
    },
}

#[derive(Parser, Debug)]
enum TrainCommand {
    /// Install Python dependencies for LoRA training
    Setup,
}

#[derive(Parser, Debug)]
enum LicenseCommand {
    /// Show license status (default when no subcommand is given)
    Status,
    /// Activate a commercial license key
    Activate {
        /// License key (FINCH-...)
        #[arg(long)]
        key: String,
    },
    /// Remove the active commercial license key
    Remove,
}

/// Build a teacher list from well-known environment variables and config files.
/// Collects ALL available keys so every provider the user has configured is available.
fn build_teachers_from_env() -> Vec<finch::config::TeacherEntry> {
    let mut teachers: Vec<finch::config::TeacherEntry> = Vec::new();
    let mut seen_providers = std::collections::HashSet::new();

    let mut add = |provider: &str, key: &str| {
        if seen_providers.contains(provider) {
            return;
        }
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
        ("OPENAI_API_KEY", "openai"),
        ("GROK_API_KEY", "grok"),
        ("XAI_API_KEY", "grok"),
        ("GEMINI_API_KEY", "gemini"),
        ("MISTRAL_API_KEY", "mistral"),
        ("GROQ_API_KEY", "groq"),
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
    std::env::set_var("ORT_LOGGING_LEVEL", "3"); // Error and Fatal only

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
        Some(Command::Worker { bind, info }) => {
            return run_worker(bind, info).await;
        }
        Some(Command::NodeInfo) => {
            return run_node_info().await;
        }
        Some(Command::Network { network_command }) => {
            return run_network_command(network_command).await;
        }
        Some(Command::License { license_command }) => {
            return run_license_command(license_command).await;
        }
        Some(Command::Agent {
            persona,
            tasks,
            reflect_every,
            once,
        }) => {
            return run_agent(persona, tasks, reflect_every, once).await;
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
    use finch::cli::global_output::{set_global_output, set_global_status};
    use finch::cli::{OutputManager, StatusBar};
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
                        // Create config from unified providers list (new format)
                        let active_theme = result.active_theme.clone();
                        let default_persona = result.default_persona.clone();
                        let daemon_only_mode = result.daemon_only_mode;
                        let mdns_discovery = result.mdns_discovery;

                        // Patch any empty API keys in the providers list with
                        // auto-detected values from environment variables.
                        let mut providers = result.providers;
                        let auto = build_teachers_from_env();
                        for p in &mut providers {
                            if let Some(key) = p.api_key() {
                                if key.is_empty() {
                                    let ptype = p.provider_type().to_string();
                                    if let Some(detected) =
                                        auto.iter().find(|t| t.provider == ptype)
                                    {
                                        // Replace the empty-key entry with a filled one
                                        *p = finch::config::ProviderEntry::from_teacher_entry(
                                            detected,
                                        );
                                    }
                                }
                            }
                        }
                        // If still no cloud providers with keys, add auto-detected ones
                        let has_keys = providers
                            .iter()
                            .any(|p| p.api_key().map(|k| !k.is_empty()).unwrap_or(false));
                        if !has_keys && !auto.is_empty() {
                            for t in &auto {
                                providers
                                    .insert(0, finch::config::ProviderEntry::from_teacher_entry(t));
                            }
                        }

                        let mut new_config = Config::with_providers(providers);
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
                            memory_context_lines: result.memory_context_lines,
                            max_verbatim_messages: new_config.features.max_verbatim_messages,
                            context_recall_k: new_config.features.context_recall_k,
                            enable_summarization: new_config.features.enable_summarization,
                            auto_compact_enabled: new_config.features.auto_compact_enabled,
                        };
                        if daemon_only_mode {
                            new_config.server.mode = "daemon-only".to_string();
                        }
                        if mdns_discovery {
                            new_config.server.advertise = true;
                            new_config.client.auto_discover = true;
                        }
                        #[allow(deprecated)]
                        {
                            new_config.streaming_enabled = new_config.features.streaming_enabled;
                        }
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
                            let names: Vec<&str> =
                                teachers.iter().map(|t| t.provider.as_str()).collect();
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

    // --cloud-only / --teacher-only: skip local model and daemon entirely
    if args.cloud_only {
        config.backend.enabled = false;
    }

    // Check for --direct or --cloud-only flags (both bypass daemon)
    // In direct/cloud-only mode: no daemon connection, talk directly to teacher API
    let use_daemon = !args.direct && !args.cloud_only;

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
        if args.cloud_only && io::stdout().is_terminal() {
            output_manager.write_status("☁️  Cloud-only mode - using teacher API (no local model)");
        } else if args.direct && io::stdout().is_terminal() {
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
    use finch::daemon::{ensure_daemon_running, DaemonLifecycle};

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
    let daemon_url = format!(
        "http://{}/health",
        finch::config::constants::DEFAULT_DAEMON_ADDR
    );

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
    println!(
        "  Bind Address:    {}",
        finch::config::constants::DEFAULT_DAEMON_ADDR
    );
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
    use std::process::Command;

    println!("\x1b[1;36m🔧 Setting up Python environment for LoRA training\x1b[0m\n");

    // Determine paths
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
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
    println!(
        "Python environment ready at: \x1b[1m{}\x1b[0m",
        venv_dir.display()
    );
    println!("\nTo use the training scripts:");
    println!("  \x1b[1;36m~/.finch/venv/bin/python scripts/train_lora.py\x1b[0m");
    println!("\nTraining will run automatically when you provide feedback.");

    Ok(())
}

async fn run_daemon(bind_address: String) -> Result<()> {
    use finch::daemon::DaemonLifecycle;
    use finch::local::LocalGenerator;
    use finch::models::{BootstrapLoader, GeneratorState, TrainingCoordinator};
    use finch::server::{AgentServer, ServerConfig};
    use finch::{output_progress, output_status};
    use std::sync::Arc;
    use tokio::sync::RwLock;

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

    let file_writer = Arc::new(log_file);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || file_writer.clone())
        .with_ansi(false); // No ANSI colors in log file

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
    std::env::set_var("ORT_LOGGING_LEVEL", "3"); // Error and Fatal only

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

    // Build the multi-provider pool from [[providers]] config (cloud providers only).
    // Falls back gracefully to the legacy ClaudeClient path when empty.
    let providers: Vec<Box<dyn finch::providers::LlmProvider>> = {
        use finch::providers::create_providers_from_entries;
        create_providers_from_entries(&config.providers).unwrap_or_default()
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
        providers,
    )?;

    // Set up mDNS service advertisement if enabled
    let service_discovery = if config.server.advertise {
        use finch::service::{ServiceConfig, ServiceDiscovery};

        let service_config = ServiceConfig {
            name: config.server.service_name.clone(),
            description: config.server.service_description.clone(),
            model: format!("{:?}", config.backend.model_size), // e.g., "Small", "Medium", "Large"
            capabilities: vec![
                "code".to_string(),
                "general".to_string(),
                "tool-use".to_string(),
            ],
        };

        match ServiceDiscovery::new(service_config) {
            Ok(discovery) => {
                // Extract port from bind_address
                let port = config
                    .server
                    .bind_address
                    .split(':')
                    .next_back()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(finch::config::constants::DEFAULT_DAEMON_PORT);

                match discovery.advertise(port) {
                    Ok(_) => {
                        tracing::info!("✓ mDNS advertisement enabled");
                        Some(discovery)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to advertise service: {}. Continuing without mDNS.",
                            e
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create service discovery: {}. Continuing without mDNS.",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Set up graceful shutdown handling
    let server_handle = tokio::spawn(async move { server.serve().await });

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

/// Build the standard tool registry + executor used for non-interactive query mode.
/// Auto-approves all tools (no interactive prompting in non-interactive mode).
async fn build_query_tool_executor() -> Result<(
    Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    Vec<finch::tools::types::ToolDefinition>,
)> {
    use finch::tools::implementations::{
        BashTool, EditTool, GlobTool, GrepTool, PatchTool, ReadTool, WebFetchTool, WriteTool,
    };
    use finch::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadTool));
    registry.register(Box::new(GlobTool));
    registry.register(Box::new(GrepTool));
    registry.register(Box::new(WebFetchTool::new()));
    registry.register(Box::new(BashTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(PatchTool));
    registry.register(Box::new(WriteTool));

    // Auto-approve everything in non-interactive mode
    let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
    let patterns_path = dirs::home_dir()
        .map(|h| h.join(".finch").join("tool_patterns.json"))
        .unwrap_or_else(|| PathBuf::from(".finch/tool_patterns.json"));

    let executor = ToolExecutor::new(registry, permissions, patterns_path)
        .context("Failed to create tool executor")?;
    let executor = Arc::new(tokio::sync::Mutex::new(executor));

    let tool_definitions = executor.lock().await.list_all_tools().await;

    Ok((executor, tool_definitions))
}

/// Run a single query with full tool support (agentic mode)
async fn run_query(query: &str) -> Result<()> {
    use finch::client::DaemonClient;
    use finch::daemon::ensure_daemon_running;

    // Load configuration
    let config = load_config()?;

    // Build tool executor (same tools as the REPL)
    let (executor, tool_definitions) = build_query_tool_executor().await?;

    // Ensure daemon is running (auto-spawn if needed)
    if let Err(e) = ensure_daemon_running(Some(&config.client.daemon_address)).await {
        eprintln!("⚠️  Daemon failed to start: {}", e);
        eprintln!("   Using teacher API directly (no local model)");
        return run_query_teacher_only(query, &config, executor, tool_definitions).await;
    }

    // Create daemon client and run full tool loop
    let daemon_config = finch::client::DaemonConfig::from_client_config(&config.client);
    let client = DaemonClient::connect(daemon_config).await?;

    let guard = executor.lock().await;
    let response = client
        .query_with_tools(query, tool_definitions, &guard)
        .await?;
    println!("{}", response);

    Ok(())
}

/// Run query using teacher API only (fallback when daemon fails), with tool support
async fn run_query_teacher_only(
    query: &str,
    config: &Config,
    executor: Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    tool_definitions: Vec<finch::tools::types::ToolDefinition>,
) -> Result<()> {
    use finch::claude::{ContentBlock, Message, MessageRequest};

    eprintln!("⚠️  Running in teacher-only mode (no local model)");

    let claude_client = create_claude_client_with_provider(config)?;
    let model = config
        .active_teacher()
        .and_then(|t| t.model.clone())
        .unwrap_or_else(|| finch::config::constants::DEFAULT_CLAUDE_MODEL.to_string());

    let mut messages = vec![Message::user(query)];

    const MAX_TURNS: usize = 25;
    for _ in 0..MAX_TURNS {
        let request = MessageRequest {
            model: model.clone(),
            max_tokens: finch::config::constants::DEFAULT_MAX_TOKENS,
            messages: messages.clone(),
            system: Some(finch::generators::claude::CODING_SYSTEM_PROMPT.to_string()),
            tools: Some(tool_definitions.clone()),
        };

        let response = claude_client.send_message(&request).await?;

        // If no tool use, print the final text and stop
        if !response.has_tool_uses() {
            println!("{}", response.text());
            return Ok(());
        }

        // Execute tool calls and collect results
        messages.push(response.to_message());

        let tool_uses = response.tool_uses();
        let mut result_blocks = Vec::new();
        for tu in &tool_uses {
            let tool_use = finch::tools::types::ToolUse {
                id: tu.id.clone(),
                name: tu.name.clone(),
                input: tu.input.clone(),
            };
            let exec_result = {
                let guard = executor.lock().await;
                guard
                    .execute_tool::<fn() -> anyhow::Result<()>>(
                        &tool_use, None, // conversation
                        None, // save_models_fn
                        None, // batch_trainer
                        None, // local_generator
                        None, // tokenizer
                        None, // repl_mode
                        None, // plan_content
                        None, // live_output
                    )
                    .await
            };
            let (content, is_error) = match exec_result {
                Ok(result) => (result.content, result.is_error),
                Err(e) => (format!("Error: {e}"), true),
            };
            result_blocks.push(ContentBlock::tool_result(
                tu.id.clone(),
                content,
                if is_error { Some(true) } else { None },
            ));
        }

        messages.push(Message::with_content("user", result_blocks));
    }

    eprintln!("⚠️  Reached max tool turns without a final answer");
    Ok(())
}

/// Run interactive setup wizard
async fn run_setup() -> Result<()> {
    use finch::cli::show_setup_wizard;
    use finch::config::Config;

    println!("Starting Shammah setup wizard...\n");

    // Run the wizard
    let result = show_setup_wizard()?;

    // Create config from unified providers list
    let mut config = Config::with_providers(result.providers);

    // Apply feature flags
    config.features = finch::config::FeaturesConfig {
        auto_approve_tools: result.auto_approve_tools,
        streaming_enabled: result.streaming_enabled,
        debug_logging: result.debug_logging,
        #[cfg(target_os = "macos")]
        gui_automation: false,
        memory_context_lines: result.memory_context_lines,
        max_verbatim_messages: config.features.max_verbatim_messages,
        context_recall_k: config.features.context_recall_k,
        enable_summarization: config.features.enable_summarization,
        auto_compact_enabled: config.features.auto_compact_enabled,
    };
    #[allow(deprecated)]
    {
        config.streaming_enabled = config.features.streaming_enabled;
    }

    // Save configuration
    config.save()?;

    println!("\n✓ Configuration saved to ~/.finch/config.toml");
    println!("  You can now run: finch");
    println!("  Or start the daemon: finch daemon\n");

    Ok(())
}

/// Show this node's identity and capabilities
async fn run_node_info() -> Result<()> {
    use finch::node::NodeInfo;

    let config = load_config().unwrap_or_else(|_| Config::new(vec![]));
    let has_teacher = config.active_teacher().is_some();
    let info = NodeInfo::load(has_teacher)?;

    println!("╔══════════════════════════════════════╗");
    println!("║           finch node info            ║");
    println!("╚══════════════════════════════════════╝");
    println!("  Node ID  : {}", info.identity.id);
    println!("  Name     : {}", info.identity.name);
    println!("  Version  : {}", info.identity.version);
    println!("  RAM      : {}GB", info.capabilities.ram_gb);
    println!("  OS       : {}", info.capabilities.os);
    if let Some(model) = &info.capabilities.local_model {
        println!("  Model    : {}", model);
    } else {
        println!("  Model    : cloud-only (teacher API)");
    }
    println!(
        "  Teacher  : {}",
        if info.capabilities.has_teacher_api {
            "configured"
        } else {
            "none"
        }
    );
    println!();
    println!("  To run as a worker node:");
    println!("    finch worker");
    println!("  To accept queries from other machines:");
    println!("    finch worker --bind 0.0.0.0:8000");

    Ok(())
}

/// Handle `finch network` subcommands
async fn run_network_command(cmd: NetworkCommand) -> Result<()> {
    use finch::network::client::RegisterDeviceRequest;
    use finch::network::{DeviceMembership, LotusClient, MembershipStatus};
    use finch::node::identity::NodeIdentity;

    let identity = NodeIdentity::load_or_create()?;
    let mut membership = DeviceMembership::load_or_create(identity.id)?;

    match cmd {
        NetworkCommand::Status => {
            println!("╔══════════════════════════════════════╗");
            println!("║       Lotus Network Status           ║");
            println!("╚══════════════════════════════════════╝");
            println!("  Device ID  : {}", identity.id);
            println!("  Name       : {}", identity.name);
            println!("  Lotus URL  : {}", membership.lotus_url);
            println!();
            match &membership.status {
                MembershipStatus::Unregistered => {
                    println!("  Status     : Not registered");
                    println!();
                    println!("  To register this device with the Lotus Network:");
                    println!("    finch network register");
                }
                MembershipStatus::Anonymous { device_token: _ } => {
                    println!("  Status     : Registered (anonymous)");
                    println!();
                    println!("  To link this device to a Lotus account:");
                    println!("    finch network join <invite-code>");
                }
                MembershipStatus::AccountMember {
                    account_id,
                    account_name,
                    ..
                } => {
                    let name = account_name.as_deref().unwrap_or("(unnamed)");
                    println!("  Status     : Account member");
                    println!("  Account    : {} ({})", name, account_id);
                }
            }
        }

        NetworkCommand::Register => {
            if membership.status.is_registered() {
                println!("This device is already registered with the Lotus Network.");
                if let MembershipStatus::AccountMember { account_id, .. } = &membership.status {
                    println!("Linked to account: {}", account_id);
                }
                return Ok(());
            }

            println!(
                "Registering device {} with Lotus Network...",
                identity.short_id()
            );
            println!("  URL: {}", membership.lotus_url);

            let client = LotusClient::new(&membership.lotus_url)?;
            match client
                .register_device(RegisterDeviceRequest {
                    device_id: identity.id,
                    fingerprint: identity.name.clone(),
                    finch_version: identity.version.clone(),
                    os: std::env::consts::OS.to_string(),
                })
                .await
            {
                Ok(resp) => {
                    membership.status = MembershipStatus::Anonymous {
                        device_token: resp.device_token,
                    };
                    membership.save()?;

                    println!("✓ Device registered successfully.");
                    println!();
                    println!("  To link to a Lotus account:");
                    println!("    finch network join <invite-code>");
                }
                Err(e) => {
                    // Registration failed — non-fatal. Finch works fine without it.
                    println!("⚠  Could not reach Lotus Network: {}", e);
                    println!();
                    println!("  finch works fine offline — registration can be retried anytime.");
                    println!("  Run `finch network register` again when the network is available.");
                }
            }
        }

        NetworkCommand::Join { invite_code } => {
            let device_token = match membership.status.device_token() {
                Some(t) => t.to_string(),
                None => {
                    anyhow::bail!(
                        "This device is not yet registered. Run `finch network register` first."
                    );
                }
            };

            println!(
                "Joining Lotus account with invite code {}...",
                &invite_code[..invite_code.len().min(6)]
            );

            let client = LotusClient::new(&membership.lotus_url)?;
            match client.join_account(&device_token, &invite_code).await {
                Ok(resp) => {
                    let account_name = resp.account_name.clone();
                    membership.status = MembershipStatus::AccountMember {
                        account_id: resp.account_id.clone(),
                        device_token,
                        account_name,
                    };
                    membership.save()?;

                    println!(
                        "✓ Joined account: {} ({})",
                        resp.account_name.as_deref().unwrap_or("(unnamed)"),
                        resp.account_id
                    );
                }
                Err(e) => {
                    println!("⚠  Could not join account: {}", e);
                    println!();
                    println!(
                        "  Check that the invite code is valid and hasn't expired (15 min TTL)."
                    );
                    println!("  Generate a new code at lotus.net and try again.");
                }
            }
        }
    }

    Ok(())
}

/// Run as a network worker node — accepts queries from external machines
async fn run_worker(bind_address: String, info_only: bool) -> Result<()> {
    use finch::node::NodeInfo;

    let config = load_config().unwrap_or_else(|_| Config::new(vec![]));
    let has_teacher = config.active_teacher().is_some();
    let info = NodeInfo::load(has_teacher)?;

    // Always show node identity when starting as worker
    println!("╔══════════════════════════════════════╗");
    println!("║         finch worker node            ║");
    println!("╚══════════════════════════════════════╝");
    println!("  Node ID  : {}", info.identity.id);
    println!("  Name     : {}", info.identity.name);
    println!("  RAM      : {}GB", info.capabilities.ram_gb);
    if let Some(model) = &info.capabilities.local_model {
        println!("  Model    : {} (loading in background)", model);
    } else {
        println!("  Model    : cloud-only — forwarding to teacher API");
    }
    println!("  Bind     : {}", bind_address);
    println!();

    if info_only {
        return Ok(());
    }

    // Start the daemon on the specified address (usually 0.0.0.0)
    println!("  Starting worker daemon...");
    println!("  Workers on your LAN can find this node via mDNS (_finch._tcp.local.)");
    println!("  Press Ctrl+C to stop.\n");

    run_daemon(bind_address).await
}

/// Handle `finch license` subcommands
async fn run_license_command(cmd: Option<LicenseCommand>) -> Result<()> {
    use finch::config::{LicenseConfig, LicenseType};
    use finch::license::validate_key;

    let mut config = load_config().unwrap_or_else(|_| finch::config::Config::new(vec![]));

    match cmd {
        None | Some(LicenseCommand::Status) => match &config.license.license_type {
            LicenseType::Commercial => {
                println!("License: Commercial ✓");
                if let Some(name) = &config.license.licensee_name {
                    if let Some(expires) = &config.license.expires_at {
                        println!("  Licensee:  {}", name);
                        println!("  Expires:   {}", expires);
                    } else {
                        println!("  Licensee:  {}", name);
                    }
                }
                println!("  Renew at:  https://polar.sh/darwin-finch");
            }
            LicenseType::Noncommercial => {
                println!("License: Noncommercial");
                println!("  Free for personal, educational, and research use.");
                println!("  Using Finch commercially? $10/yr → https://polar.sh/darwin-finch");
                println!("  Activate: finch license activate --key <key>");
            }
        },

        Some(LicenseCommand::Activate { key }) => match validate_key(&key) {
            Ok(parsed) => {
                config.license = LicenseConfig {
                    key: Some(key),
                    license_type: LicenseType::Commercial,
                    verified_at: Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
                    expires_at: Some(parsed.expires_at.format("%Y-%m-%d").to_string()),
                    licensee_name: Some(parsed.name.clone()),
                    notice_suppress_until: None,
                };
                if let Err(e) = config.save() {
                    eprintln!("⚠️  License activated but could not save config: {}", e);
                } else {
                    println!("✓ License activated");
                    println!("  Licensee:  {} ({})", parsed.name, parsed.email);
                    println!("  Expires:   {}", parsed.expires_at.format("%Y-%m-%d"));
                }
            }
            Err(e) => {
                eprintln!("✗ License activation failed: {}", e);
                std::process::exit(1);
            }
        },

        Some(LicenseCommand::Remove) => {
            config.license = LicenseConfig::default();
            if let Err(e) = config.save() {
                eprintln!("⚠️  Could not save config: {}", e);
            } else {
                println!("✓ License removed. Now using noncommercial license.");
            }
        }
    }

    Ok(())
}

/// Run the autonomous agent loop
async fn run_agent(
    persona: String,
    tasks: Option<PathBuf>,
    reflect_every: usize,
    once: bool,
) -> Result<()> {
    use finch::agent::{AgentConfig, AgentLoop};

    // Load config (needs teacher API for the agentic loop)
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {}", e);
            eprintln!("Run `finch setup` to configure a teacher API key.");
            return Err(e);
        }
    };

    if config.active_teacher().is_none() {
        anyhow::bail!(
            "No teacher API configured.\n\
             Agent mode requires a teacher API (Claude, GPT-4, etc.).\n\
             Run `finch setup` to add one."
        );
    }

    // Set up logging (stderr only, not TUI)
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .try_init();

    let tasks_path = AgentConfig::resolve_tasks_path(tasks);

    println!("finch agent");
    println!("  Tasks file : {}", tasks_path.display());
    println!("  Reflect every {} tasks", reflect_every);
    if once {
        println!("  Mode: --once (exit after first task)");
    }
    println!();

    let agent_config = AgentConfig {
        persona_spec: persona,
        tasks_path,
        reflect_every: reflect_every.max(1), // At least 1 to avoid div-by-zero
        once,
    };

    let mut agent = AgentLoop::new(config, agent_config);
    agent.run().await
}
