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

    /// Resume a previous session by UUID (printed on exit).
    /// Shorthand for --restore-session ~/.finch/sessions/<uuid>.json
    #[arg(long = "resume")]
    resume: Option<String>,

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

    /// Evaluate a typed Co-Forth expression directly through the shared VM
    /// (no AI, no REPL, and no legacy interpreter fallback).
    #[arg(long = "forth", short = 'f')]
    forth: Option<String>,

    /// Evaluate a typed Finch Lisp expression directly through the shared VM
    /// (no AI, no REPL, and no legacy evaluator fallback).
    #[arg(long = "lisp", short = 'l')]
    lisp: Option<String>,

    /// Execute a self-contained Finch Lisp or Co-Forth script through the
    /// shared typed runtime. This is the shebang target for `#!/path/to/finch
    /// --exec` and never falls back to legacy language evaluators.
    #[arg(long = "exec", value_name = "SCRIPT")]
    exec_script: Option<PathBuf>,

    /// Print the structured typed-runtime outcome for `--exec` or direct
    /// `--forth`/`--lisp` source.
    #[arg(long)]
    json: bool,

    /// Join a named session (UUIDv5 derived from name).
    /// Two users with the same name arrive at the same session ID.
    /// Example: finch --session "battleground"
    #[arg(long = "session", short = 's')]
    session: Option<String>,

    /// Connect to a remote finch peer at host:port and join their session.
    /// This machine's peer loops will exchange messages with the remote.
    /// Example: finch --peer 192.168.1.42:8000
    #[arg(long = "peer")]
    peer: Vec<String>,
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
    /// Co-Forth: run or validate Forth code
    Coforth {
        #[command(subcommand)]
        coforth_command: CoforthCommand,
    },
    /// English word library: build, list, and inspect
    Library {
        #[command(subcommand)]
        library_command: LibraryCommand,
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
    /// Exchange Forth functions with peers via a shared channel
    Exchange {
        #[command(subcommand)]
        exchange_command: ExchangeCommand,
    },
    /// Generate sample spreadsheets into ~/.finch/samples/xlsx/
    Samples,
    /// Manage saved sessions
    Sessions {
        #[command(subcommand)]
        sessions_command: SessionsCommand,
    },
}

#[derive(Parser, Debug)]
enum SessionsCommand {
    /// List saved sessions
    List,
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
enum CoforthCommand {
    /// Run Forth code and print output
    Run {
        /// Forth source code to execute
        #[arg(long)]
        code: String,
    },
    /// Validate Forth code (run and report success/failure)
    Validate {
        /// Forth source code to validate
        #[arg(long)]
        code: String,
    },
}

#[derive(Parser, Debug)]
enum LibraryCommand {
    /// List all words in the library
    List,
    /// Show a word's definition and Forth code
    Show {
        /// Word to look up
        word: String,
    },
    /// Generate Forth for English words using AI, saving to ~/.finch/library.toml
    Build {
        /// Generate all built-in word categories
        #[arg(long)]
        all: bool,
        /// Generate a specific category (run `finch library build --list-categories` to see them)
        #[arg(long)]
        category: Option<String>,
        /// Comma-separated list of specific words
        #[arg(long)]
        words: Option<String>,
        /// List available categories and exit
        #[arg(long)]
        list_categories: bool,
        /// Validate each snippet before saving (requires binary to be built)
        #[arg(long, default_value = "true")]
        validate: bool,
        /// Words per API batch
        #[arg(long, default_value = "15")]
        batch_size: usize,
        /// Max concurrent API calls (default: unlimited)
        #[arg(long)]
        forks: Option<usize>,
        /// Model override (e.g. claude-haiku-4-5-20251001 for cheap bulk generation)
        #[arg(long)]
        model: Option<String>,
        /// Write output to a file instead of ~/.finch/library.toml
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run every Forth snippet in the library and report failures
    Verify {
        /// Show passing words too (default: failures and missing only)
        #[arg(long)]
        verbose: bool,
    },
    /// Generate Forth for words that are missing snippets or have broken ones
    Heal {
        /// Words per API batch
        #[arg(long, default_value = "15")]
        batch_size: usize,
        /// Max concurrent API calls (default: unlimited)
        #[arg(long)]
        forks: Option<usize>,
        /// Model override (e.g. claude-haiku-4-5-20251001 for cheap bulk generation)
        #[arg(long)]
        model: Option<String>,
        /// Write output to a file instead of ~/.finch/library.toml
        #[arg(long)]
        output: Option<PathBuf>,
    },
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

#[derive(Parser, Debug)]
enum ExchangeCommand {
    /// Propose a Forth function to the channel
    Propose {
        /// Word name (e.g. next-prime)
        name: String,
        /// Forth source code for the word (e.g. ": next-prime ... ;")
        code: String,
        /// Channel to post to (default: #exchange)
        #[arg(long, default_value = "#exchange")]
        channel: String,
        /// Daemon address (default: 127.0.0.1:11435)
        #[arg(long)]
        daemon: Option<String>,
    },
    /// List all proposals in the channel
    List {
        /// Channel to inspect (default: #exchange)
        #[arg(long, default_value = "#exchange")]
        channel: String,
        /// Daemon address (default: 127.0.0.1:11435)
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Execute all proposals in the channel on this machine
    Run {
        /// Channel to execute (default: #exchange)
        #[arg(long, default_value = "#exchange")]
        channel: String,
        /// Daemon address (default: 127.0.0.1:11435)
        #[arg(long)]
        daemon: Option<String>,
    },
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

/// Execute a Finch script using only the shared typed runtime. Script headers
/// select syntax but never grant authority; non-pure operations therefore
/// return the normal typed authorization outcome instead of using a shell or
/// legacy interpreter as an escape hatch.
async fn run_finch_script(path: PathBuf, json_output: bool) -> Result<()> {
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("read Finch script '{}'", path.display()))?;
    let script = finch::programs::parse_finch_script(&path, &contents)?;
    let runtime = finch::runtime::ProgramRuntime::new();
    // Executing a local script is the user's explicit request to receive its
    // response.  Grant only that presentation capability here; every resource
    // capability (files, processes, network, automation, and so on) remains
    // subject to the ordinary typed broker.
    runtime.grant_typed_capability(finch::vm::CapabilityRequirement {
        capability: finch::vm::CapabilityKind::SessionEmit,
        selector: finch::vm::ResourceSelector::None,
    })?;
    let outcome = runtime
        .submit_typed_only(finch::runtime::ProgramSubmission {
            language: script.language,
            source_id: Some(path.display().to_string()),
            source: script.source,
            intent: format!("execute Finch script {}", path.display()),
            // The typed verifier and broker derive the concrete capabilities.
            // This legacy coarse field is intentionally not used to authorize
            // a typed-only script.
            effect: finch::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: None,
            budget: None,
        })
        .await?;

    if json_output {
        // Keep stdout machine-readable even for a failed/paused program, but
        // do not turn a typed failure into a successful CI invocation.
        println!("{}", serde_json::to_string(&outcome)?);
    } else if let Some(presentation) = terminal_script_presentation(&outcome.output) {
        // `say` is append-only at the VM boundary.  The command-line host,
        // rather than the language primitive, owns the final terminal line
        // break so an interactive shell prompt cannot join the last fragment.
        print!("{presentation}");
    }
    if !matches!(
        outcome.status,
        finch::runtime::outcome::ExecutionStatus::Completed
    ) {
        let detail = if outcome.required_capabilities.is_empty() {
            outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| "no diagnostic".to_string())
        } else {
            format!(
                "requires capability grant(s): {:?}",
                outcome.required_capabilities
            )
        };
        anyhow::bail!(
            "Finch script did not complete ({:?}): {}",
            outcome.status,
            detail
        );
    }
    Ok(())
}

/// Adapt one completed script's append-only response stream to a terminal.
/// Other hosts project the same `session.emit` events into output handles and
/// must not inherit this terminal-only trailing newline rule.
fn terminal_script_presentation(output: &str) -> Option<String> {
    (!output.is_empty()).then(|| {
        if output.ends_with('\n') {
            output.to_owned()
        } else {
            format!("{output}\n")
        }
    })
}

/// Evaluate source supplied directly at the command line through the same
/// typed runtime as scripts and provider wire responses.  This deliberately
/// does not fall back to either legacy interpreter: a direct Lisp/Co-Forth
/// program must have the same verifier, capabilities, and diagnostics as an
/// LLM-authored program.
async fn run_direct_typed_source(
    language: finch::programs::ProgramLanguage,
    source: &str,
) -> Result<()> {
    run_direct_typed_source_with_json(language, source, false).await
}

/// Variant of [`run_direct_typed_source`] for non-interactive callers. It
/// serializes the same `ExecutionOutcome` used by shebang-style `--exec`, so
/// direct Co-Forth is not a second text-only result protocol.
async fn run_direct_typed_source_with_json(
    language: finch::programs::ProgramLanguage,
    source: &str,
    json_output: bool,
) -> Result<()> {
    let runtime = finch::runtime::ProgramRuntime::new();
    runtime.grant_typed_capability(finch::vm::CapabilityRequirement {
        capability: finch::vm::CapabilityKind::SessionEmit,
        selector: finch::vm::ResourceSelector::None,
    })?;
    let outcome = runtime
        .submit_typed_only(finch::runtime::ProgramSubmission {
            language,
            source_id: Some(format!("direct-cli.{}", language.as_str())),
            source: source.to_string(),
            intent: "direct typed command-line program".to_string(),
            effect: finch::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: None,
            budget: None,
        })
        .await?;

    if json_output {
        println!("{}", serde_json::to_string(&outcome)?);
    } else if let Some(presentation) = terminal_script_presentation(&outcome.output) {
        print!("{presentation}");
    }
    if outcome.status == finch::runtime::outcome::ExecutionStatus::Completed {
        return Ok(());
    }
    let detail = outcome
        .diagnostics
        .first()
        .cloned()
        .unwrap_or_else(|| format!("program ended as {:?}", outcome.status));
    anyhow::bail!(
        "typed {} program did not complete: {detail}",
        language.as_str()
    )
}

#[cfg(test)]
mod script_tests {
    use super::*;

    #[test]
    fn shebang_style_exec_arguments_parse_as_a_script_invocation() {
        let args = Args::try_parse_from(["finch", "--exec", "reply.lisp", "--json"]).unwrap();
        assert_eq!(args.exec_script, Some(PathBuf::from("reply.lisp")));
        assert!(args.json);
    }

    #[test]
    fn direct_forth_json_arguments_parse_as_a_typed_invocation() {
        let args = Args::try_parse_from(["finch", "--forth", "1 2 +", "--json"]).unwrap();
        assert_eq!(args.forth.as_deref(), Some("1 2 +"));
        assert!(args.json);
    }

    #[test]
    fn direct_lisp_json_arguments_parse_as_a_typed_invocation() {
        let args = Args::try_parse_from(["finch", "--lisp", "(+ 1 2)", "--json"]).unwrap();
        assert_eq!(args.lisp.as_deref(), Some("(+ 1 2)"));
        assert!(args.json);
    }

    #[tokio::test]
    async fn executable_script_uses_the_typed_runtime_and_rejects_legacy_only_forth() {
        let script = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            script.path(),
            "#!/usr/bin/env finch --exec --language=lisp\n(begin (say \"script ready\") (+ 20 22))\n",
        )
        .unwrap();
        run_finch_script(script.path().to_path_buf(), false)
            .await
            .unwrap();

        std::fs::write(
            script.path(),
            "#!/usr/bin/env finch --exec --language=forth\n: legacy-only 1 ;\n",
        )
        .unwrap();
        let error = run_finch_script(script.path().to_path_buf(), false)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("E-FORTH-SIG-001"),
            "expected typed-only rejection, got: {error:#}"
        );

        // JSON is a presentation mode, not a success override: automation
        // callers must receive a non-zero result when the typed program did
        // not complete, while still being able to consume the JSON outcome.
        let error = run_finch_script(script.path().to_path_buf(), true)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("E-FORTH-SIG-001"),
            "expected JSON-mode typed rejection, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn executable_lisp_script_expands_bounded_typed_syntax_templates() {
        let script = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            script.path(),
            "#!/usr/bin/env finch --exec --language=lisp\n\
             (define-syntax (answer value) (+ value 1))\n\
             (say (int-to-string (answer 41)))\n",
        )
        .unwrap();

        run_finch_script(script.path().to_path_buf(), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn direct_forth_uses_the_typed_runtime_and_rejects_legacy_definitions() {
        run_direct_typed_source(
            finch::programs::ProgramLanguage::Forth,
            "6 7 * int-to-string say",
        )
        .await
        .unwrap();

        let error =
            run_direct_typed_source(finch::programs::ProgramLanguage::Forth, ": legacy-only 1 ;")
                .await
                .unwrap_err();
        assert!(
            error.to_string().contains("E-FORTH-SIG-001"),
            "expected typed-only rejection, got: {error:#}"
        );
    }

    #[test]
    fn terminal_script_adapter_terminates_only_a_nonempty_unfinished_response() {
        assert_eq!(terminal_script_presentation(""), None);
        assert_eq!(
            terminal_script_presentation("The answer is 42"),
            Some("The answer is 42\n".into())
        );
        assert_eq!(
            terminal_script_presentation("already complete\n"),
            Some("already complete\n".into())
        );
    }

    #[test]
    fn one_shot_wire_repair_is_limited_to_static_vm_diagnostics() {
        assert!(is_repairable_one_shot_wire_diagnostic(
            "E-READ-004: missing quote"
        ));
        assert!(is_repairable_one_shot_wire_diagnostic(
            "E-LINK-002: unknown word"
        ));
        assert!(is_repairable_one_shot_wire_diagnostic(
            "E-WIRE-002: Markdown code fence"
        ));
        assert!(!is_repairable_one_shot_wire_diagnostic(
            "E-LIMIT-001: fuel exhausted"
        ));

        let request = one_shot_wire_repair_request("Hello!", "E-LINK-002: unknown word");
        assert!(request.contains("exactly one complete raw Finch Lisp or Co-Forth program"));
        assert!(request.contains("Hello!"));
        assert!(request.contains("E-LINK-002"));
    }

    #[test]
    fn daemon_and_teacher_one_shot_paths_share_the_vm_wire_contract() {
        let prompt = vm_wire_system_prompt();
        assert!(
            prompt.contains("Every text response is exactly one complete executable Finch program")
        );
        assert!(prompt.contains("Default to Lisp"));
    }

    #[tokio::test]
    async fn one_shot_wire_receiver_executes_a_daemon_style_final_response() {
        let runtime = finch::runtime::ProgramRuntime::new();
        let outcome = execute_one_shot_wire_source(&runtime, "(say \"daemon wire executed\")")
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            finch::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.output, "daemon wire executed");
    }

    #[test]
    fn prose_about_a_forth_string_opener_is_not_executed_as_forth() {
        assert!(!is_clearly_forth(
            "Return only a raw Co-Forth program that uses standard .\" output shorthand."
        ));
        assert!(is_clearly_forth(".\" Forth shorthand works\""));
        assert!(is_clearly_forth("s\"Forth strings are data\" say"));
    }

    #[tokio::test]
    async fn query_tools_and_terminal_wire_share_one_typed_runtime() {
        use finch::tools::types::ToolContext;
        use finch::tools::ToolRegistry;

        let runtime = Arc::new(finch::runtime::ProgramRuntime::new());
        let mut registry = ToolRegistry::new();
        register_query_vm_tools(&mut registry, Arc::clone(&runtime));
        for name in [
            "submit_program",
            "get_vm_state",
            "get_language_definition",
            "search_vm_vocabulary",
            "inspect_vm_word",
            "search_word",
            "inspect_word",
        ] {
            assert!(registry.has_tool(name), "missing query VM tool {name}");
        }

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            stack: None,
            poset: None,
        };
        let before = runtime.revision();
        registry
            .get("submit_program")
            .unwrap()
            .execute(
                serde_json::json!({
                    "language": "lisp",
                    "source": "(+ 20 22)",
                    "intent": "query runtime sharing regression test",
                    "effect": "pure",
                    "manifest_generation": runtime.manifest_generation(),
                    "expected_revision": before,
                }),
                &context,
            )
            .await
            .unwrap();
        assert!(runtime.revision() > before);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Suppress ONNX Runtime verbose logs BEFORE any initialization
    // Must be set early, before any ONNX library code runs
    // ORT_LOGGING_LEVEL: 0=Verbose, 1=Info, 2=Warning, 3=Error, 4=Fatal
    std::env::set_var("ORT_LOGGING_LEVEL", "3"); // Error and Fatal only

    // Install panic handler to cleanup terminal on panic
    install_panic_handler();

    // Parse command-line arguments
    let args = Args::parse();
    // `Command::Query` is dispatched before the REPL setup below, so preserve
    // this global flag explicitly rather than accidentally dropping it on the
    // one-shot path.
    let cloud_only = args.cloud_only;

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
            return run_query(&query, cloud_only).await;
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
        Some(Command::Coforth { coforth_command }) => {
            return run_coforth_command(coforth_command);
        }
        Some(Command::Library { library_command }) => {
            return run_library_command(library_command).await;
        }
        Some(Command::Agent {
            persona,
            tasks,
            reflect_every,
            once,
        }) => {
            return run_agent(persona, tasks, reflect_every, once).await;
        }
        Some(Command::Exchange { exchange_command }) => {
            return run_exchange_command(exchange_command).await;
        }
        Some(Command::Samples) => {
            return run_samples();
        }
        Some(Command::Sessions { sessions_command }) => {
            return run_sessions_command(sessions_command);
        }
        None => {
            // Fall through to REPL mode (check for piped input first)
        }
    }

    if let Some(script) = args.exec_script {
        return run_finch_script(script, args.json).await;
    }

    // --forth: direct typed Co-Forth evaluation, no AI, TUI, or config
    // needed. The explicit `coforth` maintenance subcommand remains the
    // legacy interpreter's home; provider-facing/direct source must not gain
    // a bypass around the shared verifier and capability broker.
    if let Some(forth_expr) = &args.forth {
        return run_direct_typed_source_with_json(
            finch::programs::ProgramLanguage::Forth,
            forth_expr,
            args.json,
        )
        .await;
    }

    if let Some(lisp_expr) = &args.lisp {
        return run_direct_typed_source_with_json(
            finch::programs::ProgramLanguage::Lisp,
            lisp_expr,
            args.json,
        )
        .await;
    }

    if args.json {
        anyhow::bail!("--json requires --exec <SCRIPT>, --forth <SOURCE>, or --lisp <SOURCE>");
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
        return run_query(input.trim(), cloud_only).await;
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
                use crossterm::style::Stylize as _;
                eprintln!(
                    "\n{}",
                    format!("✓ Auto-configured: {}", names.join(", "))
                        .green()
                        .bold()
                );
                eprintln!(
                    "{}\n",
                    "  Run `finch setup` any time to change settings.".yellow()
                );
                let cfg = Config::new(auto_teachers);
                cfg.save().ok();
                cfg
            } else {
                {
                    use crossterm::execute;
                    use crossterm::style::{
                        Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor,
                    };
                    let _ = execute!(
                        std::io::stderr(),
                        Print("\n"),
                        SetForegroundColor(Color::Yellow),
                        SetAttribute(Attribute::Bold),
                        Print("⚠️  Running first-time setup wizard..."),
                        ResetColor,
                        Print("\n\n"),
                    );
                }

                // Run setup wizard
                use finch::cli::show_setup_wizard;
                match show_setup_wizard() {
                    Ok(result) => {
                        // Create config from unified providers list (new format)
                        let active_theme = result.active_theme.clone();
                        let default_persona = result.default_persona.clone();
                        let daemon_only_mode = result.daemon_only_mode;
                        let mdns_discovery = result.mdns_discovery;
                        let finch_api_key = result.finch_api_key.clone();

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
                        finch::cli::setup_wizard::apply_daemon_api_key(
                            &mut new_config,
                            &finch_api_key,
                        );
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
                            brain_enabled: new_config.features.brain_enabled,
                        };
                        if daemon_only_mode {
                            new_config.server.mode = "daemon-only".to_string();
                        }
                        if mdns_discovery {
                            new_config.server.advertise = true;
                        }
                        new_config.client.auto_discover = result.auto_discover;
                        #[allow(deprecated)]
                        {
                            new_config.streaming_enabled = new_config.features.streaming_enabled;
                        }
                        new_config.save()?;
                        use crossterm::style::Stylize as _;
                        eprintln!("\n{}\n", "✓ Configuration saved!".green().bold());
                        new_config
                    }
                    Err(wizard_err) if wizard_err.to_string().contains("Setup cancelled") => {
                        // User pressed Escape/Ctrl+C — don't crash, fall back gracefully
                        use crossterm::style::Stylize as _;
                        eprintln!(
                            "\n{}",
                            "Setup skipped. Detecting API keys from environment...".yellow()
                        );

                        let teachers = build_teachers_from_env();

                        if teachers.is_empty() {
                            eprintln!("{}", "No API keys found. Set ANTHROPIC_API_KEY (or OPENAI_API_KEY / GROK_API_KEY)".yellow());
                            eprintln!(
                                "{}\n",
                                "and re-run, or run `finch setup` to configure interactively."
                                    .yellow()
                            );
                        } else {
                            let names: Vec<&str> =
                                teachers.iter().map(|t| t.provider.as_str()).collect();
                            eprintln!(
                                "{}\n",
                                format!("✓ Auto-configured: {}", names.join(", ")).green()
                            );
                        }

                        let cfg = Config::new(teachers);
                        // Save so next launch doesn't show the wizard again
                        if cfg.save().is_err() {
                            // Non-fatal — we'll just show the wizard again next time
                        }
                        {
                            use crossterm::execute;
                            use crossterm::style::{
                                Attribute, Color, Print, ResetColor, SetAttribute,
                                SetForegroundColor,
                            };
                            let _ = execute!(
                                std::io::stderr(),
                                Print("\n"),
                                SetForegroundColor(Color::Green),
                                SetAttribute(Attribute::Bold),
                                Print("✓ Setup complete!"),
                                ResetColor,
                                Print(" Type "),
                                SetAttribute(Attribute::Bold),
                                Print("/help"),
                                SetAttribute(Attribute::Reset),
                                Print(" for commands, or just start talking.\n\n"),
                            );
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
            api_key: config.server.api_keys.first().cloned(),
        };
        match DaemonClient::connect(daemon_config).await {
            Ok(client) => Some(Arc::new(client)),
            Err(_e) => {
                tracing::debug!("Failed to connect to daemon: {}", _e);
                None
            }
        }
    } else {
        None
    };

    // Create and run REPL (with full TUI support)
    // Pass daemon_client so Repl knows whether to suppress local model logs
    // Session name and ID.
    // --session <name>: join that named session (deterministic UUIDv5 from name).
    // default: generate a cute name (quiet-hill etc.) and derive a UUID.
    // Suppressed for pipe / non-interactive use.
    let (session_name, session_id) = {
        use finch::session::names;

        let (name, id) = match &args.session {
            Some(given) => {
                let id = names::to_uuid(given);
                (given.clone(), id)
            }
            None => {
                let name = names::generate();
                let id = names::to_uuid(&name);
                (name, id)
            }
        };

        // If a daemon is running, register / join the session so other terminals
        // with the same name can share the broadcast channel.
        if let Some(ref client) = daemon_client {
            let url = format!("{}/v1/session/join", client.base_url());
            let body = serde_json::json!({ "name": name });
            // Fire-and-forget — failure just means no cross-terminal broadcast.
            let _ = reqwest::Client::new()
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await;
        }

        (name, id)
    };
    let _ = (session_name, session_id); // available for future use (daemon routing, shared stacks)

    let mut repl = Repl::new(config, claude_client, router, metrics_logger, daemon_client).await;

    if !args.peer.is_empty() {
        repl.set_peers(args.peer.clone());
    }

    // Resolve --resume <uuid> → --restore-session ~/.finch/sessions/<uuid>.json
    let restore_session = args.restore_session.or_else(|| {
        args.resume.as_deref().and_then(|uuid| {
            dirs::home_dir().map(|h| {
                h.join(".finch")
                    .join("sessions")
                    .join(format!("{uuid}.json"))
            })
        })
    });

    // Restore session if requested
    if let Some(session_path) = restore_session {
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

    // Run inside a LocalSet so IpcClient (capnp-rpc, !Send) can use spawn_local.
    // Normal tokio::spawn calls inside the event loop still go to the thread pool.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Try IPC connection to the daemon socket (non-blocking).
            // If it fails (daemon not running), we just skip it.
            if let Ok(ipc) = finch::ipc::IpcClient::connect().await {
                repl.set_ipc_client(ipc);
            }
            repl.run_event_loop(args.initial_prompt).await
        })
        .await?;

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
    // mdns_sd=error: suppress WARN "No buffer space available" on VPN/tunnel interfaces —
    // those interfaces don't support multicast; the error is harmless noise.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mdns_sd=error"));

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
        print_daemon_client_details(&bind_address);
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
    print_daemon_client_details(&bind_address);

    Ok(())
}

fn print_daemon_client_details(bind_address: &str) {
    let client_address = bind_address
        .strip_prefix("0.0.0.0:")
        .map(|port| format!("127.0.0.1:{port}"))
        .unwrap_or_else(|| bind_address.to_string());
    println!("\nOpenAI-compatible clients (Roo Code, Cline, etc.):");
    println!("  Base URL: http://{client_address}/v1");
    println!("  Models:   http://{client_address}/v1/models");

    if let Ok(config) = load_config() {
        let names: Vec<String> = config.providers.iter().map(|p| p.profile_name()).collect();
        if !names.is_empty() {
            println!("  Model ID: {}", names.join(", "));
        }
        if config.server.auth_enabled {
            println!("  API key:  required (the Finch client key from Settings)");
        } else {
            println!("  API key:  not required (use any placeholder if your client requires one)");
        }
    }
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
        use crossterm::style::Stylize as _;
        println!("{}", "⚠ Daemon is not running".yellow().bold());
        println!("\nStart the daemon with:");
        println!("  {}", "finch daemon-start".cyan().bold());
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
    use crossterm::style::Stylize as _;
    println!("{}", "✓ Daemon Status".green().bold());
    println!();
    println!("  Status:          {}", health.status.green().bold());
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

    use crossterm::style::Stylize as _;
    println!(
        "{}\n",
        "🔧 Setting up Python environment for LoRA training"
            .cyan()
            .bold()
    );

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
    println!("\n{}\n", "✅ Setup complete!".green().bold());
    println!(
        "Python environment ready at: {}",
        venv_dir.display().to_string().bold()
    );
    println!("\nTo use the training scripts:");
    println!(
        "  {}",
        "~/.finch/venv/bin/python scripts/train_lora.py"
            .cyan()
            .bold()
    );
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
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mdns_sd=error"));

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
        brain_password: config.server.brain_password.clone(),
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
    let server = Arc::new(server);
    let server_handle = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.serve().await }
    });

    // Start Cap'n Proto IPC server on Unix socket (internal CLI ↔ daemon channel).
    // capnp-rpc uses !Send futures, so we run it on a dedicated single-threaded runtime
    // inside a spawn_blocking thread rather than tokio::spawn (which requires Send).
    let ipc_handle = tokio::task::spawn_blocking({
        let server = Arc::clone(&server);
        move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("IPC tokio runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(finch::ipc::start_ipc_server(server)))
        }
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
        result = ipc_handle => {
            match result {
                Ok(Ok(())) => tracing::info!("IPC server exited normally"),
                Ok(Err(e)) => tracing::error!(error = %e, "IPC server error"),
                Err(e) => tracing::error!(error = %e, "IPC server task panicked"),
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
async fn build_query_tool_executor(
    config: &Config,
) -> Result<(
    Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    Vec<finch::tools::types::ToolDefinition>,
    Arc<finch::runtime::ProgramRuntime>,
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

    let program_runtime = Arc::new(finch::runtime::ProgramRuntime::new());
    register_query_vm_tools(&mut registry, Arc::clone(&program_runtime));

    // Auto-approve everything in non-interactive mode
    let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
    let patterns_path = dirs::home_dir()
        .map(|h| h.join(".finch").join("tool_patterns.json"))
        .unwrap_or_else(|| PathBuf::from(".finch/tool_patterns.json"));

    let executor = ToolExecutor::new(registry, permissions, patterns_path)
        .context("Failed to create tool executor")?
        .with_mcp(config)
        .await;
    let executor = Arc::new(tokio::sync::Mutex::new(executor));

    let tool_definitions = executor.lock().await.list_all_tools().await;

    Ok((executor, tool_definitions, program_runtime))
}

/// Install the typed-VM discovery and execution tools used by a one-shot
/// provider loop. The final raw wire response receives this same runtime.
fn register_query_vm_tools(
    registry: &mut finch::tools::ToolRegistry,
    program_runtime: Arc<finch::runtime::ProgramRuntime>,
) {
    use finch::tools::implementations::{
        GetLanguageDefinitionTool, GetVmStateTool, InspectWordTool, SearchWordTool,
        SubmitProgramTool,
    };

    // Provider tool calls and the terminal VM-wire program must share this
    // exact runtime. Otherwise a model can inspect or define a word through
    // `submit_program`, then have its final raw Lisp/Co-Forth response run in
    // a different empty stack/dictionary.
    registry.register(Box::new(SubmitProgramTool::new(Arc::clone(
        &program_runtime,
    ))));
    registry.register(Box::new(GetVmStateTool::new(Arc::clone(&program_runtime))));
    registry.register(Box::new(GetLanguageDefinitionTool));
    // One-shot query mode has no loaded persisted-program index, but the
    // canonical tools still expose core words and report that limitation.
    registry.register(Box::new(SearchWordTool::new(
        Arc::clone(&program_runtime),
        None,
    )));
    registry.register(Box::new(InspectWordTool::new(program_runtime, None)));
    registry.register_alias("search_vm_vocabulary", "search_word");
    registry.register_alias("inspect_vm_word", "inspect_word");
    registry.register_alias("search_vocabulary", "search_word");
    registry.register_alias("inspect_program", "inspect_word");
}

/// Returns true when the input is unambiguously Forth code that should bypass
/// the AI entirely and run directly in the co-forth VM.
///
/// Matches:
/// - `: name body ;`  — word definition
/// - Any `keyword"` string-literal openers used by the co-forth tokeniser
/// - Stack expressions: every token is a number, operator char, or known Forth word
fn is_clearly_forth(s: &str) -> bool {
    let t = s.trim();
    if t.starts_with(": ") || t.starts_with("; ") || t.starts_with(":require ") {
        return true;
    }
    // Forth string-literal openers: keyword followed immediately by `"`
    const OPENERS: &[&str] = &[
        "hash\"",
        "open\"",
        "eval\"",
        "space\"",
        "csv\"",
        "tsv\"",
        "xlsx\"",
        "read\"",
        "exec\"",
        "glob\"",
        "gen\"",
        "confirm\"",
        "select\"",
        ".\"",
        "s\"",
        "boot\"",
        "call\"",
        "scatter\"",
        "say\"",
        "join\"",
        "part\"",
        "contribute\"",
        "run-on\"",
        "require\"",
        "xlsx-into\"",
    ];
    for opener in OPENERS {
        if t.starts_with(opener) {
            return true;
        }
    }
    // Natural language disqualifiers: question marks, apostrophes (contractions),
    // commas, or an uppercase-starting word that isn't a standalone token of digits.
    if t.contains('?') || t.contains(',') {
        return false;
    }
    if t.starts_with(|c: char| c.is_uppercase()) {
        return false;
    }
    // Forth operator characters that have no place in natural language
    const FORTH_OP_CHARS: &[char] = &['+', '*', '@', '!', ';', '<', '>', '='];
    if FORTH_OP_CHARS.iter().any(|&c| t.contains(c)) {
        return true;
    }
    // Pure stack expression: every whitespace token is a number, a standalone `-`,
    // `/`, `.`, or `.s`, or a known Forth primitive word.
    const FORTH_PRIMITIVES: &[&str] = &[
        ".", ".s", "cr", "space", "dup", "drop", "swap", "over", "rot", "nip", "tuck", "2dup",
        "2drop", "mod", "abs", "max", "min", "negate", "and", "or", "xor", "invert", "words",
        "help", "depth", "bye", "emit", "type", "i", "j", "-", "/",
    ];
    let tokens: Vec<&str> = t.split_whitespace().collect();
    if !tokens.is_empty()
        && tokens.iter().all(|tok| {
            tok.parse::<f64>().is_ok()
                || FORTH_PRIMITIVES.contains(tok)
                || tok
                    .chars()
                    .all(|c| matches!(c, '+' | '-' | '*' | '/' | '.' | '@' | '!' | '<' | '>' | '='))
        })
    {
        return true;
    }
    false
}

/// Run a single query with full tool support (agentic mode)
async fn run_query(query: &str, cloud_only: bool) -> Result<()> {
    use finch::client::DaemonClient;
    use finch::daemon::ensure_daemon_running;

    // Short-circuit: typed Lisp expressions start with `(` — before Forth check.
    if query.trim_start().starts_with('(') {
        println!("{}", query);
        run_direct_typed_source(finch::programs::ProgramLanguage::Lisp, query).await?;
        return Ok(());
    }

    // Short-circuit: run typed Co-Forth directly, no AI involved.
    if is_clearly_forth(query) {
        println!("{}", query);
        run_direct_typed_source(finch::programs::ProgramLanguage::Forth, query).await?;
        return Ok(());
    }

    // Load configuration
    let config = load_config()?;

    // Build tool executor (same tools as the REPL)
    let (executor, tool_definitions, program_runtime) = build_query_tool_executor(&config).await?;

    // A one-shot cloud-only query must not first attempt the daemon. Besides
    // defeating the flag, that startup attempt can consume the whole caller
    // timeout and makes direct-provider smoke tests look hung.
    if cloud_only {
        return run_query_teacher_only(query, &config, executor, tool_definitions, program_runtime)
            .await;
    }

    // Ensure daemon is running (auto-spawn if needed)
    if let Err(e) = ensure_daemon_running(Some(&config.client.daemon_address)).await {
        eprintln!("⚠️  Daemon failed to start: {}", e);
        eprintln!("   Using teacher API directly (no local model)");
        return run_query_teacher_only(query, &config, executor, tool_definitions, program_runtime)
            .await;
    }

    // Create daemon client and run full tool loop
    let daemon_config = finch::client::DaemonConfig::from_client_config(&config.client);
    let client = DaemonClient::connect(daemon_config).await?;

    let guard = executor.lock().await;
    let response = client
        .query_with_tools_with_system(
            query,
            Some(vm_wire_system_prompt()),
            tool_definitions,
            &guard,
        )
        .await?;
    // The daemon owns model inference and tool-loop routing, but this CLI
    // process owns the local typed runtime.  A final text response is therefore
    // still Finch wire source, never user-facing prose to print verbatim.
    // Running it here keeps the daemon and --cloud-only paths semantically
    // identical without handing workspace/UI authority to the daemon.
    let outcome = match execute_one_shot_wire_source(&program_runtime, &response).await {
        Ok(outcome) if outcome.status == finch::runtime::outcome::ExecutionStatus::Completed => {
            print!("{}", outcome.output);
            return Ok(());
        }
        Ok(outcome) if can_repair_one_shot_wire_outcome(&outcome) => {
            let diagnostic = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            // A correction is source-only.  Do not give it the tool manifest:
            // a malformed, effect-free response must not turn into a new
            // arbitrary host action merely because it is being repaired.
            let repair = client
                .query_with_tools_with_system(
                    &one_shot_wire_repair_request(&response, &diagnostic),
                    Some(vm_wire_system_prompt()),
                    Vec::new(),
                    &guard,
                )
                .await?;
            execute_one_shot_wire_source(&program_runtime, &repair).await?
        }
        Ok(outcome) => outcome,
        Err(error) if is_repairable_one_shot_wire_diagnostic(&error.to_string()) => {
            let repair = client
                .query_with_tools_with_system(
                    &one_shot_wire_repair_request(&response, &error.to_string()),
                    Some(vm_wire_system_prompt()),
                    Vec::new(),
                    &guard,
                )
                .await?;
            execute_one_shot_wire_source(&program_runtime, &repair).await?
        }
        Err(error) => return Err(error),
    };
    if outcome.status == finch::runtime::outcome::ExecutionStatus::Completed {
        print!("{}", outcome.output);
    } else {
        let diagnostic = outcome
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
        anyhow::bail!(
            "daemon provider VM-wire program ended as {:?}: {}",
            outcome.status,
            diagnostic
        );
    }

    Ok(())
}

/// Run query using teacher API only (fallback when daemon fails), with tool support
async fn run_query_teacher_only(
    query: &str,
    config: &Config,
    executor: Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    tool_definitions: Vec<finch::tools::types::ToolDefinition>,
    program_runtime: Arc<finch::runtime::ProgramRuntime>,
) -> Result<()> {
    use finch::claude::{ContentBlock, Message, MessageRequest};

    eprintln!("⚠️  Running in teacher-only mode (no local model)");

    let claude_client = create_claude_client_with_provider(config)?;
    let model = config
        .active_teacher()
        .and_then(|t| t.model.clone())
        .unwrap_or_else(|| finch::config::constants::DEFAULT_CLAUDE_MODEL.to_string());

    let mut messages = vec![Message::user(query)];
    // Keep one-shot provider calls on the same wire contract as the REPL.
    // Otherwise `finch --cloud-only query` is a misleading test surface: it
    // asks the provider for ordinary prose and never validates a VM program.
    let system = vm_wire_system_prompt();

    const MAX_TURNS: usize = 25;
    let mut wire_repair_requested = false;
    for _ in 0..MAX_TURNS {
        let request = MessageRequest {
            model: model.clone(),
            max_tokens: finch::config::constants::DEFAULT_MAX_TOKENS,
            messages: messages.clone(),
            system: Some(system.clone()),
            tools: (!wire_repair_requested).then(|| tool_definitions.clone()),
        };

        let response = claude_client.send_message(&request).await?;

        // A text-only reply is the same raw Lisp/Co-Forth wire program used
        // by the interactive client. Execute it rather than displaying source
        // as though it were an ordinary chat response.
        if !response.has_tool_uses() {
            let source = response.text();
            let outcome = match execute_one_shot_wire_source(&program_runtime, &source).await {
                Ok(outcome) => outcome,
                Err(error)
                    if !wire_repair_requested
                        && is_repairable_one_shot_wire_diagnostic(&error.to_string()) =>
                {
                    messages.push(response.to_message());
                    messages.push(Message::user(one_shot_wire_repair_request(
                        &source,
                        &error.to_string(),
                    )));
                    wire_repair_requested = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if outcome.status == finch::runtime::outcome::ExecutionStatus::Completed {
                print!("{}", outcome.output);
                return Ok(());
            }

            let diagnostic = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            if !wire_repair_requested && can_repair_one_shot_wire_outcome(&outcome) {
                messages.push(response.to_message());
                messages.push(Message::user(one_shot_wire_repair_request(
                    &source,
                    &diagnostic,
                )));
                wire_repair_requested = true;
                continue;
            }
            anyhow::bail!(
                "provider VM-wire program ended as {:?}: {}",
                outcome.status,
                diagnostic
            );
        }

        if wire_repair_requested {
            anyhow::bail!("provider used tools while repairing a rejected Finch VM wire program");
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
                        None, // stack
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

/// The same provider-facing contract accompanies every one-shot transport.
/// Keep it out of an ordinary user message so a user request cannot be
/// confused with the wire protocol itself.
fn vm_wire_system_prompt() -> String {
    const VM_WIRE_BOOT: &str = include_str!("../vocabulary/BOOT.md");
    format!(
        "{}\n\n{}",
        finch::generators::claude::CODING_SYSTEM_PROMPT,
        VM_WIRE_BOOT
    )
}

/// Submit one completed one-shot provider response through the same typed VM
/// receiver used by every CLI transport.  The caller decides whether a failed,
/// effect-free program merits the single repair turn; this helper never retries
/// or renders raw source as prose.
async fn execute_one_shot_wire_source(
    program_runtime: &finch::runtime::ProgramRuntime,
    source: &str,
) -> Result<finch::runtime::outcome::ExecutionOutcome> {
    let language = finch::programs::ProgramLanguage::infer_wire_source(source)?;
    program_runtime
        .submit_typed_only(finch::runtime::ProgramSubmission {
            language,
            source_id: Some(format!("provider-response.{}", language.as_str())),
            source: source.to_string(),
            intent: "one-shot provider VM-wire response".to_string(),
            effect: finch::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: program_runtime.manifest_generation(),
            expected_revision: Some(program_runtime.revision()),
            budget: None,
        })
        .await
}

/// One-shot provider calls use the same conservative repair boundary as the
/// interactive VM-wire receiver: only a rejected, effect-free source program
/// may be corrected once.  Execution, approval, and partial-effect outcomes
/// are never replayed merely because a model can generate another response.
fn can_repair_one_shot_wire_outcome(outcome: &finch::runtime::outcome::ExecutionOutcome) -> bool {
    use finch::runtime::outcome::ExecutionStatus;

    outcome.status == ExecutionStatus::Failed
        && outcome.side_effects.is_empty()
        && outcome.vm_side_effects.is_empty()
        && outcome.effect_journal.is_empty()
        && outcome
            .vm_diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .chain(outcome.diagnostics.iter().map(String::as_str))
            .any(is_repairable_one_shot_wire_diagnostic)
}

fn is_repairable_one_shot_wire_diagnostic(diagnostic: &str) -> bool {
    matches!(
        diagnostic,
        value if value.starts_with("E-READ-")
            || value.starts_with("E-TYPE-")
            || value.starts_with("E-STACK-")
            || value.starts_with("E-LISP-")
            || value.starts_with("E-FORTH-")
            || value.starts_with("E-LINK-")
            || value.starts_with("E-CAP-")
            || value.starts_with("E-WIRE-")
    )
}

fn one_shot_wire_repair_request(rejected_source: &str, diagnostic: &str) -> String {
    format!(
        "The preceding Finch VM wire program was rejected before execution. \
         Re-emit exactly one complete raw Finch Lisp or Co-Forth program; do not use Markdown, prose, or tools.\n\n\
         Rejected source:\n---\n{rejected_source}\n---\n\
         Diagnostic:\n{diagnostic}"
    )
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
    finch::cli::setup_wizard::apply_daemon_api_key(&mut config, &result.finch_api_key);

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
        brain_enabled: config.features.brain_enabled,
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

// ── finch coforth ─────────────────────────────────────────────────────────────

fn execute_coforth_code(code: &str) -> Result<String> {
    // Use the pre-compiled VM so major words and the full library are available.
    // Do not run legacy `boot` entries implicitly: they can print poetry or run
    // proof demonstrations before the requested program, and startup work must
    // be an explicit, reviewed BrainRun rather than ambient VM behavior.
    let mut vm = finch::coforth::Library::precompiled_vm();
    vm.exec(code)?;
    Ok(std::mem::take(&mut vm.out))
}

fn run_coforth_command(cmd: CoforthCommand) -> Result<()> {
    match cmd {
        CoforthCommand::Run { code } => match execute_coforth_code(&code) {
            Ok(out) => print!("{out}"),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        },
        CoforthCommand::Validate { code } => match execute_coforth_code(&code) {
            Ok(out) if !out.is_empty() => {
                println!("ok  →  {:?}", out.trim());
            }
            Ok(_) => {
                eprintln!("fail: compiled and ran but produced no output");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("fail: {e}");
                std::process::exit(1);
            }
        },
    }
    Ok(())
}

// ── finch library ─────────────────────────────────────────────────────────────

/// Build an AI generator from the teacher config, with an optional model override.
fn make_generator(
    teachers: &[finch::config::TeacherEntry],
    model_override: Option<&str>,
) -> Result<std::sync::Arc<dyn finch::generators::Generator>> {
    let mut teachers_owned: Vec<finch::config::TeacherEntry> = teachers.to_vec();
    if let Some(m) = model_override {
        for t in &mut teachers_owned {
            t.model = Some(m.to_string());
        }
    }
    let provider = finch::providers::create_provider(&teachers_owned)?;
    let client = std::sync::Arc::new(finch::claude::ClaudeClient::with_provider(provider));
    Ok(std::sync::Arc::new(
        finch::generators::claude::ClaudeGenerator::new(client),
    ))
}

async fn run_library_command(cmd: LibraryCommand) -> Result<()> {
    use finch::coforth::generator::{self, BuildOptions, CATEGORIES};
    use finch::coforth::Library;

    match cmd {
        LibraryCommand::List => {
            let lib = Library::load();
            let words = lib.all_words();
            println!("{} words in library:", words.len());
            for w in &words {
                print!("{w}  ");
            }
            println!();
        }

        LibraryCommand::Show { word } => {
            let lib = Library::load();
            match lib.lookup(&word) {
                Some(e) => {
                    println!("word:       {}", e.word);
                    println!("definition: {}", e.definition);
                    println!("kind:       {}", e.kind);
                    println!("related:    {}", e.related.join(", "));
                    if let Some(ref forth) = e.forth {
                        println!("forth:      {forth}");
                        // Run it to show output
                        if let Ok(out) = finch::coforth::Forth::run(forth) {
                            if !out.is_empty() {
                                println!("output:     {}", out.trim());
                            }
                        }
                    }
                }
                None => {
                    eprintln!("'{}' not found in library", word);
                    std::process::exit(1);
                }
            }
        }

        LibraryCommand::Verify { verbose } => {
            let lib = Library::load();
            let mut ok = 0usize;
            let mut missing = 0usize;
            let mut broken: Vec<(String, String)> = Vec::new();

            let mut words: Vec<&str> = lib.word_list();
            words.sort_unstable();

            for word in &words {
                let senses = lib.lookup_all(word);
                for entry in senses {
                    match &entry.forth {
                        None => {
                            missing += 1;
                            if verbose {
                                println!("  ? {word}  (no Forth)");
                            }
                        }
                        Some(code) => match finch::coforth::Forth::run(code) {
                            Ok(out) if !out.is_empty() => {
                                ok += 1;
                                if verbose {
                                    println!("  ✓ {word}");
                                }
                            }
                            Ok(_) => {
                                broken.push((word.to_string(), "no output".to_string()));
                            }
                            Err(e) => {
                                broken.push((word.to_string(), e.to_string()));
                            }
                        },
                    }
                }
            }

            println!();
            println!("Library: {} words", lib.word_count());
            println!("  ✓ verified:  {ok}");
            println!("  ? no Forth:  {missing}");
            println!("  ✗ broken:    {}", broken.len());

            if !broken.is_empty() {
                println!();
                println!("Broken snippets:");
                for (w, e) in &broken {
                    println!("  ✗ {w}: {e}");
                }
            }

            if missing > 0 && !verbose {
                println!();
                println!(
                    "Run `finch library heal` to generate Forth for the {missing} missing words."
                );
            }

            if !broken.is_empty() || missing > 0 {
                std::process::exit(1);
            }
        }

        LibraryCommand::Heal {
            batch_size,
            forks,
            model,
            output,
        } => {
            // Collect words that are missing Forth or have broken snippets
            let lib = Library::load();
            let mut words_to_heal: Vec<String> = Vec::new();

            for word in lib.word_list() {
                let senses = lib.lookup_all(word);
                for entry in senses {
                    let needs_healing = match &entry.forth {
                        None => true,
                        Some(code) => match finch::coforth::Forth::run(code) {
                            Ok(out) if out.is_empty() => true,
                            Err(_) => true,
                            _ => false,
                        },
                    };
                    if needs_healing {
                        let key = if let Some(ref s) = entry.sense {
                            format!("{}:{}", word, s)
                        } else {
                            word.to_string()
                        };
                        words_to_heal.push(key);
                    }
                }
            }

            if words_to_heal.is_empty() {
                println!(
                    "All {} words already have verified Forth snippets.",
                    lib.word_count()
                );
                return Ok(());
            }

            println!("{} words need Forth snippets.", words_to_heal.len());

            let config = load_config().unwrap_or_else(|_| finch::config::Config::new(vec![]));
            let gen: std::sync::Arc<dyn finch::generators::Generator> =
                match make_generator(&config.teachers, model.as_deref()) {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("No provider configured: {e}");
                        eprintln!("Run `finch setup` to configure an API key.");
                        std::process::exit(1);
                    }
                };

            let output_path = output.unwrap_or_else(generator::user_library_path);
            let opts = generator::BuildOptions {
                all: false,
                category: None,
                words: Some(words_to_heal),
                batch_size,
                forks,
                validate: true,
                force: true,
                output: output_path,
            };
            generator::build_library(opts, gen).await?;
        }

        LibraryCommand::Build {
            all,
            category,
            words,
            list_categories,
            validate,
            batch_size,
            forks,
            model,
            output,
        } => {
            if list_categories {
                println!("Available categories ({}):", CATEGORIES.len());
                for (name, words) in CATEGORIES {
                    println!("  {:30} ({} words)", name, words.len());
                }
                return Ok(());
            }

            let words_vec: Option<Vec<String>> =
                words.map(|w| w.split(',').map(|s| s.trim().to_lowercase()).collect());

            let output_path = output.unwrap_or_else(generator::user_library_path);

            // Build a generator from config, with optional model override
            let config = load_config().unwrap_or_else(|_| finch::config::Config::new(vec![]));
            let gen: std::sync::Arc<dyn finch::generators::Generator> =
                match make_generator(&config.teachers, model.as_deref()) {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("No provider configured: {e}");
                        eprintln!("Run `finch setup` to configure an API key.");
                        std::process::exit(1);
                    }
                };

            let opts = BuildOptions {
                all,
                category,
                words: words_vec,
                batch_size,
                forks,
                validate,
                force: false,
                output: output_path,
            };

            generator::build_library(opts, gen).await?;
        }
    }

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

fn run_samples() -> Result<()> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".finch")
        .join("samples")
        .join("xlsx");

    finch::samples::generate_all(&dir)?;

    println!("Sample spreadsheets written to {}:", dir.display());
    for name in &[
        "grades.xlsx",
        "budget.xlsx",
        "contacts.xlsx",
        "times_table.xlsx",
    ] {
        println!("  {}", dir.join(name).display());
    }
    println!();
    println!("Try in the REPL:");
    println!(
        "  s\" {}/grades.xlsx\" s\" A2\" xlsx@ type cr",
        dir.display()
    );
    println!(
        "  s\" {}/times_table.xlsx\" s\" H8\" xlsx@ type cr",
        dir.display()
    );
    Ok(())
}

/// Exchange Forth functions with peers via a shared channel on the daemon.
///
/// Workflow for two Claude Code sessions:
///   Session A: finch exchange propose next-prime ": next-prime ( n -- p ) ..."
///   Session B: finch exchange list
///   Session B: finch exchange propose next-prime ": next-prime ( n -- p ) ..."  (their version)
///   Either:    finch exchange run
async fn run_exchange_command(cmd: ExchangeCommand) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    match cmd {
        ExchangeCommand::Propose {
            name,
            code,
            channel,
            daemon,
        } => {
            let addr = daemon
                .as_deref()
                .unwrap_or(finch::config::constants::DEFAULT_DAEMON_ADDR);
            let chan = if channel.starts_with('#') {
                channel.clone()
            } else {
                format!("#{channel}")
            };
            let url = format!("http://{addr}/v1/forth/channel/{}/contribute", &chan[1..]);

            let body = serde_json::json!({
                "from": name,
                "program": code,
            });

            let resp = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .with_context(|| {
                    format!("Could not reach daemon at {addr}.\nStart it with: finch daemon-start")
                })?;

            if resp.status().is_success() {
                println!("✓ Proposed '{}' to {}", name, chan);
                println!("  Peers can see it with:  finch exchange list");
                println!("  Peers can run it with:  finch exchange run");
            } else {
                anyhow::bail!(
                    "Daemon returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
        }

        ExchangeCommand::List { channel, daemon } => {
            let addr = daemon
                .as_deref()
                .unwrap_or(finch::config::constants::DEFAULT_DAEMON_ADDR);
            let chan = if channel.starts_with('#') {
                channel.clone()
            } else {
                format!("#{channel}")
            };
            let url = format!("http://{addr}/v1/forth/channel/{}", &chan[1..]);

            let resp = client.get(&url).send().await.with_context(|| {
                format!("Could not reach daemon at {addr}.\nStart it with: finch daemon-start")
            })?;

            if !resp.status().is_success() {
                anyhow::bail!(
                    "Daemon returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }

            #[derive(serde::Deserialize)]
            struct Entry {
                from: String,
                program: String,
            }
            #[derive(serde::Deserialize)]
            struct State {
                channel: String,
                contributions: Vec<Entry>,
            }

            let state: State = resp.json().await.context("Failed to parse channel state")?;

            if state.contributions.is_empty() {
                println!("{} is empty.", state.channel);
                println!("  Propose a function with:  finch exchange propose <word> \"<code>\"");
            } else {
                println!(
                    "{}  ({} contribution{})",
                    state.channel,
                    state.contributions.len(),
                    if state.contributions.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                );
                println!();
                for (i, entry) in state.contributions.iter().enumerate() {
                    println!("  [{}] from: {}", i + 1, entry.from);
                    for line in entry.program.lines() {
                        println!("      {}", line);
                    }
                    println!();
                }
                println!("  Run all with:  finch exchange run");
            }
        }

        ExchangeCommand::Run { channel, daemon } => {
            let addr = daemon
                .as_deref()
                .unwrap_or(finch::config::constants::DEFAULT_DAEMON_ADDR);
            let chan = if channel.starts_with('#') {
                channel.clone()
            } else {
                format!("#{channel}")
            };
            let url = format!("http://{addr}/v1/forth/channel/{}/execute", &chan[1..]);

            let resp = client.post(&url).send().await.with_context(|| {
                format!("Could not reach daemon at {addr}.\nStart it with: finch daemon-start")
            })?;

            if !resp.status().is_success() {
                anyhow::bail!(
                    "Daemon returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }

            let result: serde_json::Value = resp
                .json()
                .await
                .context("Failed to parse execute response")?;

            if let Some(err) = result
                .get("error")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                eprintln!("error: {}", err);
            }
            if let Some(out) = result
                .get("output")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                print!("{}", out);
            }
            if let Some(stack) = result.get("stack") {
                println!("stack: {}", stack);
            }
        }
    }

    Ok(())
}

/// Handle `finch sessions` subcommands
fn run_sessions_command(cmd: SessionsCommand) -> Result<()> {
    match cmd {
        SessionsCommand::List => {
            let sessions_dir = dirs::home_dir()
                .map(|h| h.join(".finch").join("sessions"))
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

            if !sessions_dir.exists() {
                println!("No saved sessions.");
                return Ok(());
            }

            let mut entries: Vec<_> = std::fs::read_dir(&sessions_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x == "json")
                        .unwrap_or(false)
                })
                .collect();

            if entries.is_empty() {
                println!("No saved sessions.");
                return Ok(());
            }

            // Sort newest first by modification time.
            entries.sort_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            entries.reverse();

            println!("{:<38}  {}", "UUID", "Saved");
            println!("{}", "-".repeat(60));
            for entry in &entries {
                let path = entry.path();
                let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                    .map(|d| {
                        let secs = d.as_secs();
                        let dt = chrono::DateTime::<chrono::Local>::from(
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
                        );
                        dt.format("%Y-%m-%d %H:%M").to_string()
                    })
                    .unwrap_or_else(|| "?".to_string());
                println!("{uuid:<38}  {mtime}");
            }
            println!();
            println!("Resume with: finch --resume <uuid>");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{execute_coforth_code, register_query_vm_tools};
    use std::sync::Arc;

    #[test]
    fn coforth_command_does_not_run_legacy_boot_entries_implicitly() {
        let output = execute_coforth_code("1 2 + .").expect("program executes");
        assert_eq!(output, "3 ");
    }

    #[test]
    fn query_manifest_advertises_only_canonical_vm_discovery_tools() {
        let mut registry = finch::tools::ToolRegistry::new();
        register_query_vm_tools(
            &mut registry,
            Arc::new(finch::runtime::ProgramRuntime::new()),
        );
        let names = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<std::collections::HashSet<_>>();

        assert!(names.contains("search_word"));
        assert!(names.contains("inspect_word"));
        for legacy in [
            "search_vm_vocabulary",
            "inspect_vm_word",
            "search_vocabulary",
            "inspect_program",
        ] {
            assert!(!names.contains(legacy));
            assert!(registry.has_tool(legacy));
        }
    }
}
