// finch - terminal coding assistant
// Main entry point

use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use finch::claude::ClaudeClient;
use finch::cli::OutputManagerLayer;
use finch::cli::Repl;
use finch::config::{load_config, Config};
use finch::metrics::MetricsLogger;
use finch::models::ThresholdRouter;
use finch::router::Router;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "finch")]
#[command(about = finch::ABOUT, version)]
struct Args {
    /// Run mode
    #[command(subcommand)]
    command: Option<Command>,

    /// Initial prompt to send after startup (REPL mode)
    #[arg(long = "initial-prompt")]
    initial_prompt: Option<String>,

    /// Retired UUID session restore. Hidden so help advertises `finch attach`.
    /// The flag is still parsed so old invocations get a migration error
    /// instead of "unexpected argument". Existing `~/.finch/sessions` files
    /// are not deleted.
    #[arg(long = "restore-session", hide = true)]
    restore_session: Option<PathBuf>,

    /// Retired UUID resume flag. Hidden; see `--restore-session`.
    #[arg(long = "resume", hide = true)]
    resume: Option<String>,

    /// Use raw terminal mode instead of TUI (enables rustyline)
    #[arg(long = "raw", conflicts_with = "no_tui")]
    raw_mode: bool,

    /// Alias for --raw (for backwards compatibility)
    #[arg(long = "no-tui")]
    no_tui: bool,

    /// Direct mode - talk directly to the cloud provider API, bypass daemon
    #[arg(long = "direct")]
    direct: bool,

    /// Cloud-only mode - skip local model entirely, use the cloud provider
    /// API directly. No model download, no daemon. Great for machines without
    /// much RAM, or when you only have a cloud API key (e.g. Grok via X Premium+).
    #[arg(long = "cloud-only")]
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

    /// Compatibility alias for `finch attach <name>`. Hidden from help.
    #[arg(long = "brain", hide = true)]
    brain: Option<String>,

    /// One-shot model overlay for this invocation. Does not persist on the Brain.
    #[arg(long = "model", value_name = "ID")]
    model: Option<String>,

    /// Persist this provider entry on the current Brain (account/backend switch).
    #[arg(long = "provider", value_name = "NAME")]
    provider: Option<String>,
}

#[derive(Parser, Debug)]
enum Command {
    /// Run interactive setup wizard
    Setup,
    /// Manage Finch-native provider authentication
    Auth {
        #[command(subcommand)]
        auth_command: AuthCommand,
    },
    /// Run HTTP daemon server
    Daemon {
        /// Bind address (default: 127.0.0.1:8000)
        // constant: crate::config::DEFAULT_HTTP_ADDR
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
        /// Print each raw provider VM program to stderr before Finch executes it
        #[arg(long)]
        show_program: bool,
    },
    /// Run as a network worker node (accepts queries from other machines)
    ///
    /// Binds to 0.0.0.0 by default so other machines on the network can
    /// delegate work to this node. Shows node identity and capabilities.
    Worker {
        /// Bind address (default: 0.0.0.0:8000 — accepts external connections)
        // constant: crate::config::DEFAULT_WORKER_ADDR
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
    /// Capture/replay evidence for the Finch provider wire protocol
    WireCorpus {
        #[command(subcommand)]
        wire_corpus_command: WireCorpusCommand,
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
    /// Generate sample spreadsheets into ~/.finch/samples/xlsx/
    Samples,
    /// Attach to a named Brain through its canonical snapshot/event stream
    Attach {
        /// Brain name (`1-64` letters, numbers, `-` or `_`)
        name: String,
        /// One-shot model overlay for this invocation. Does not persist.
        #[arg(long = "model", value_name = "ID")]
        model: Option<String>,
        /// Persist this provider entry on the named Brain.
        #[arg(long = "provider", value_name = "NAME")]
        provider: Option<String>,
    },
    /// Inspect the effective global default and optional Brain provider/model
    Status {
        /// Named Brain to inspect (`finch status --brain <name>`)
        #[arg(long = "brain", value_name = "NAME")]
        brain: Option<String>,
    },
    /// List leftover legacy UUID session files
    Sessions {
        #[command(subcommand)]
        sessions_command: SessionsCommand,
    },
    /// List and archive named Brains
    Brain {
        #[command(subcommand)]
        brain_command: BrainCommand,
    },
}

#[derive(Parser, Debug)]
enum AuthCommand {
    /// Show local, secret-free authentication status without network access
    Status {
        #[arg(default_value = "chatgpt", value_parser = ["chatgpt", "grok-sub"])]
        provider: String,
        #[arg(
            long,
            default_value = "chatgpt:default",
            default_value_if("provider", "grok-sub", Some("grok-sub:default")),
            value_parser = parse_credential_reference
        )]
        credential: String,
    },
    /// Start Finch-native ChatGPT or SuperGrok device login
    Login {
        #[arg(default_value = "chatgpt", value_parser = ["chatgpt", "grok-sub"])]
        provider: String,
        #[arg(
            long,
            default_value = "chatgpt:default",
            default_value_if("provider", "grok-sub", Some("grok-sub:default")),
            value_parser = parse_credential_reference
        )]
        credential: String,
        /// Copy the one-time code to the clipboard
        #[arg(long)]
        copy: bool,
        /// Explicitly open the sign-in URL in the default browser
        #[arg(long)]
        open: bool,
    },
    /// Revoke a named subscription credential and retain a local tombstone
    Logout {
        #[arg(default_value = "chatgpt", value_parser = ["chatgpt", "grok-sub"])]
        provider: String,
        #[arg(
            long,
            default_value = "chatgpt:default",
            default_value_if("provider", "grok-sub", Some("grok-sub:default")),
            value_parser = parse_credential_reference
        )]
        credential: String,
    },
    /// Recover an interrupted local mutation as a signed-out tombstone
    Recover {
        #[arg(default_value = "chatgpt", value_parser = ["chatgpt", "grok-sub"])]
        provider: String,
        #[arg(
            long,
            default_value = "chatgpt:default",
            default_value_if("provider", "grok-sub", Some("grok-sub:default")),
            value_parser = parse_credential_reference
        )]
        credential: String,
    },
}

fn parse_credential_reference(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Err(
            "credential reference must use 1-128 ASCII letters, digits, '-', '_', or ':'"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

#[derive(Parser, Debug)]
enum SessionsCommand {
    /// List leftover legacy UUID session files
    List,
}

#[derive(Parser, Debug)]
enum BrainCommand {
    /// List named Brains without hydrating their logs
    #[command(visible_alias = "list")]
    Ls {
        /// Emit a JSON array of Brain summaries (name, path, turns, size,
        /// attachments, live agents)
        #[arg(long)]
        json: bool,
    },
    /// Archive a named Brain out of the live namespace
    ///
    /// The log is moved to `~/.finch/brains-archive/`, not deleted. Refuses
    /// while the daemon is running so a live store cannot recreate an empty
    /// Brain under the same name.
    #[command(visible_alias = "remove")]
    Rm {
        /// Brain name (`1-64` letters, numbers, `-` or `_`)
        name: String,
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
enum WireCorpusCommand {
    /// Compile and verify a versioned JSONL corpus without executing it
    Audit {
        /// Corpus written through FINCH_WIRE_CORPUS_PATH
        corpus: PathBuf,
        /// Emit the aggregate report as JSON
        #[arg(long)]
        json: bool,
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

/// Build a cloud provider list from well-known environment variables and
/// config files. Collects ALL available keys so every provider the user has
/// configured is available.
fn build_cloud_providers_from_env() -> Vec<finch::config::ProviderEntry> {
    let mut providers: Vec<finch::config::ProviderEntry> = Vec::new();
    let mut seen_providers = std::collections::HashSet::new();

    let mut add = |provider: &str, key: &str| {
        if seen_providers.contains(provider) {
            return;
        }
        seen_providers.insert(provider.to_string());
        providers.push(finch::config::ProviderEntry::from_provider_fields(
            provider,
            key.trim().to_string(),
            None,
            None,
            None,
        ));
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

    providers
}

fn first_run_setup_cancelled() -> anyhow::Error {
    anyhow::anyhow!("Setup cancelled; no configuration was saved")
}

async fn finish_first_run_setup<V, VF, L, W>(
    wizard_result: Result<finch::cli::SetupResult>,
    validate_and_apply: V,
    load_saved_config: L,
    success_output: &mut W,
) -> Result<Config>
where
    V: FnOnce(finch::cli::SetupResult) -> VF,
    VF: std::future::Future<Output = Result<finch::cli::SetupApplyOutcome>>,
    L: FnOnce() -> Result<Config>,
    W: std::io::Write + ?Sized,
{
    let result = match wizard_result {
        Ok(result) => result,
        Err(error) if error.to_string().contains("Setup cancelled") => {
            return Err(first_run_setup_cancelled());
        }
        Err(error) => return Err(error),
    };

    if validate_and_apply(result).await? == finch::cli::SetupApplyOutcome::Cancelled {
        return Err(first_run_setup_cancelled());
    }

    let config = load_saved_config()?;
    use crossterm::style::Stylize as _;
    writeln!(
        success_output,
        "\n{}\n",
        "✓ Configuration saved!".green().bold()
    )?;
    Ok(config)
}

/// Create a ClaudeClient with the configured provider
///
/// This function creates a provider from the configured cloud providers
/// and wraps it in a ClaudeClient for backwards compatibility.
fn create_claude_client_with_provider(config: &Config) -> Result<ClaudeClient> {
    let graph = finch::providers::create_provider_graph_from_config(config)?;
    Ok(ClaudeClient::with_shared_provider(graph.default_provider()))
}

/// Execute a Finch script using only the shared typed runtime. Script headers
/// select syntax but never grant authority; non-pure operations therefore
/// return the normal typed authorization outcome instead of using a shell or
/// legacy interpreter as an escape hatch.
async fn run_finch_script(path: PathBuf, json_output: bool) -> Result<()> {
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("read Finch script '{}'", path.display()))?;
    let script = finch_programs::parse_finch_script(&path, &contents)?;
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
            effect: finch_programs::ExecutionEffect::Unclassified,
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
    if !matches!(outcome.status, finch::runtime::ExecutionStatus::Completed) {
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
    language: finch_programs::ProgramLanguage,
    source: &str,
) -> Result<()> {
    run_direct_typed_source_with_json(language, source, false).await
}

/// Variant of [`run_direct_typed_source`] for non-interactive callers. It
/// serializes the same `ExecutionOutcome` used by shebang-style `--exec`, so
/// direct Co-Forth is not a second text-only result protocol.
async fn run_direct_typed_source_with_json(
    language: finch_programs::ProgramLanguage,
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
            effect: finch_programs::ExecutionEffect::Unclassified,
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
    if outcome.status == finch::runtime::ExecutionStatus::Completed {
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
            finch_programs::ProgramLanguage::Forth,
            "6 7 * int-to-string say",
        )
        .await
        .unwrap();

        let error =
            run_direct_typed_source(finch_programs::ProgramLanguage::Forth, ": legacy-only 1 ;")
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
        assert!(request.contains("It was forth; repair it as forth"));
        assert!(request.contains("exactly one complete raw Finch forth ProgramSubmission"));
        assert!(request.contains("Hello!"));
        assert!(request.contains("E-LINK-002"));
    }

    #[test]
    fn daemon_and_cloud_one_shot_paths_share_the_vm_wire_contract() {
        let prompt = vm_wire_system_prompt();
        assert!(prompt.contains("complete body of every text response is one"));
        assert!(prompt.contains("`ProgramSubmission`"));
        assert!(prompt.contains("Default to Lisp"));
        assert!(prompt.contains("two structurally different provider outputs"));
        assert!(prompt.contains("issue tool calls without a prose"));
        assert!(prompt.contains("preamble, then return raw Lisp/Co-Forth source"));
        assert!(prompt.contains("every tool-result"));
        assert!(prompt.contains("continuation, is instead a complete `ProgramSubmission`"));
    }

    #[test]
    fn one_shot_wire_metrics_preserve_first_failure_and_repair_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let logger = finch::metrics::MetricsLogger::new(dir.path().to_path_buf()).unwrap();
        let mut metric =
            finch::metrics::WireAdherenceMetric::first_pass("xai", "grok-code-fast-1", "one_shot");
        mark_wire_rejection(&mut metric, "Hello there!", "E-LINK-002: unknown word");
        metric.repair_attempted = true;
        finish_wire_metric(Some(&logger), &mut metric, Some("hello"), false);

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let recorded = logger.read_wire_metrics(&today).unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].failure_class,
            Some(finch::metrics::WireFailureClass::RawProse)
        );
        assert!(recorded[0].repaired_successfully);
        assert!(!recorded[0].terminal_failure);
    }

    #[test]
    fn wire_corpus_audit_command_parses_without_starting_the_repl() {
        let args = Args::try_parse_from([
            "finch",
            "wire-corpus",
            "audit",
            "fixtures/provider-wire.jsonl",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(Command::WireCorpus {
                wire_corpus_command: WireCorpusCommand::Audit { corpus, json: true }
            }) if corpus == PathBuf::from("fixtures/provider-wire.jsonl")
        ));
    }

    #[tokio::test]
    async fn one_shot_wire_receiver_executes_a_daemon_style_final_response() {
        let runtime = finch::runtime::ProgramRuntime::new();
        let outcome = execute_one_shot_wire_source(&runtime, "(say \"daemon wire executed\")")
            .await
            .unwrap();
        assert_eq!(outcome.status, finch::runtime::ExecutionStatus::Completed);
        assert_eq!(outcome.output, "daemon wire executed");
    }

    #[test]
    fn test_contraction_with_emphasis_is_not_executed_as_forth() {
        // `don't!` contains `!`, which is a Forth operator character, so before #571 the operator
        // test claimed it and the typed runtime failed on an unknown word. The apostrophe has to
        // disqualify the line first.
        for prose in ["don't!", "can't!", "won't!", "it's >50"] {
            assert!(
                !is_clearly_forth(prose),
                "prose with a contraction must route to natural language: {prose:?}"
            );
        }
    }

    #[test]
    fn test_a_forth_line_with_an_operator_still_runs() {
        // The guard above must not swallow real programs: `!` is how Co-Forth stores.
        for program in ["42 counter !", "1 2 +", "dup @"] {
            assert!(
                is_clearly_forth(program),
                "a typed program must still be recognised: {program:?}"
            );
        }
    }

    #[test]
    fn mention_tokens_in_query_are_not_executed_as_forth() {
        for prose in ["explain @src/foo.rs", "@src/foo.rs"] {
            assert!(
                !is_clearly_forth(prose),
                "composer mentions must route to query assembly, not Co-Forth: {prose:?}"
            );
        }
        assert!(
            is_clearly_forth("dup @"),
            "standalone Forth fetch @ must still run as a typed program"
        );
        assert!(
            is_clearly_forth("5 @"),
            "standalone Forth fetch @ after a number must still run as a typed program"
        );
    }

    #[test]
    fn test_a_forth_string_may_still_contain_an_apostrophe() {
        // A string opener is matched before any disqualifier, so quoting prose still works.
        assert!(is_clearly_forth("s\"it's fine\" say"));
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
        use finch::tools::ToolContext;
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
            save_models: None,
            plan_content: None,
            live_output: None,
            host_mode_state: None,
            effect_audit: None,
            skip_interactive_review: false,
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

/// Suppress ONNX Runtime's verbose native logging by default, without
/// overriding an operator's own `ORT_LOGGING_LEVEL`.
///
/// Both call sites used to hardcode this to `"3"` unconditionally, which made
/// it impossible to get ORT's own diagnostic output (0=Verbose..4=Fatal) for
/// debugging a CoreML/ORT session-construction problem (#223) — setting the
/// env var before launching had no effect because this line stomped it right
/// back. Must still run before any ONNX library code, same as before.
fn suppress_ort_logs_unless_overridden() {
    if std::env::var_os("ORT_LOGGING_LEVEL").is_none() {
        std::env::set_var("ORT_LOGGING_LEVEL", "3"); // Error and Fatal only
    }
}

fn run_selection_status(brain: Option<String>) -> Result<()> {
    let config = load_config()?;
    let global = config
        .default_provider_name()
        .unwrap_or_else(|| "(none)".into());
    println!("global default: {global}");
    if let Some(name) = brain {
        let name = finch::brain::BrainStore::validate_name(&name)?.to_string();
        let store = finch::brain::BrainStore::new("local");
        let persisted = store.provider_selection(&name)?;
        let request = finch::cli::SelectionRequest {
            default_provider: config.default_provider_name(),
            persisted,
            cli_provider: None,
            cli_model: None,
        };
        match finch::cli::resolve_selection(&config.providers, &request) {
            Ok(effective) => println!("{}", effective.status_report(None)),
            Err(error) => {
                println!("brain: {name}");
                println!("error: {error}");
            }
        }
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Start the startup clock first, so t0 is as close to process entry as a
    // statement in `main` can be (#364).
    finch::startup::begin();

    // Suppress ONNX Runtime verbose logs BEFORE any initialization, unless the
    // operator asked for more (#223 diagnostics). Must be set early, before
    // any ONNX library code runs.
    suppress_ort_logs_unless_overridden();

    // Install panic handler to cleanup terminal on panic
    install_panic_handler();

    // Parse command-line arguments
    let mut args = {
        let _phase = finch::startup::phase(finch::startup::PHASE_ARGS);
        Args::parse()
    };
    reject_retired_session_flags(&args)?;
    // `finch attach NAME` is the canonical REPL entry; take it so the
    // subcommand match below falls through to interactive mode.
    let attach_brain = match args.command.take() {
        Some(Command::Attach {
            name,
            model,
            provider,
        }) => {
            if args.model.is_none() {
                args.model = model;
            }
            if args.provider.is_none() {
                args.provider = provider;
            }
            Some(finch::brain::BrainStore::validate_name(&name)?.to_string())
        }
        other => {
            args.command = other;
            None
        }
    };
    // `Command::Query` is dispatched before the REPL setup below, so preserve
    // this global flag explicitly rather than accidentally dropping it on the
    // one-shot path.
    let cloud_only = args.cloud_only;

    // Dispatch based on command
    match args.command {
        Some(Command::Setup) => {
            return run_setup().await;
        }
        Some(Command::Auth { auth_command }) => {
            return run_auth_command(auth_command).await;
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
        Some(Command::Query {
            query,
            show_program,
        }) => {
            return run_query(&query, cloud_only, show_program).await;
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
        Some(Command::WireCorpus {
            wire_corpus_command,
        }) => {
            return run_wire_corpus_command(wire_corpus_command);
        }
        Some(Command::Agent {
            persona,
            tasks,
            reflect_every,
            once,
        }) => {
            return run_agent(persona, tasks, reflect_every, once).await;
        }
        Some(Command::Samples) => {
            return run_samples();
        }
        Some(Command::Sessions { sessions_command }) => {
            return run_sessions_command(sessions_command);
        }
        Some(Command::Brain { brain_command }) => {
            return run_brain_command(brain_command);
        }
        Some(Command::Status { brain }) => {
            return run_selection_status(brain);
        }
        Some(Command::Attach { .. }) | None => {
            // `finch attach NAME` is applied above via `attach_brain`.
            // Fall through to REPL mode (check for piped input first).
        }
    }

    if let Some(script) = args.exec_script {
        return run_finch_script(script, args.json).await;
    }

    // --forth: direct typed Co-Forth evaluation, no AI, TUI, or config.
    // All public source enters the shared verifier and capability broker.
    if let Some(forth_expr) = &args.forth {
        return run_direct_typed_source_with_json(
            finch_programs::ProgramLanguage::Forth,
            forth_expr,
            args.json,
        )
        .await;
    }

    if let Some(lisp_expr) = &args.lisp {
        return run_direct_typed_source_with_json(
            finch_programs::ProgramLanguage::Lisp,
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
        return run_query(input.trim(), cloud_only, false).await;
    }

    // CRITICAL: Create and configure OutputManager BEFORE initializing tracing
    // This prevents lazy initialization with stdout enabled
    use finch::cli::{set_global_output, set_global_status};
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
    //
    // This is the first of four full reads and TOML parses of `config.toml`
    // per interactive start; the timing separates it from the load that
    // actually produces the configuration (#364).
    {
        let mut phase = finch::startup::phase(finch::startup::PHASE_CONFIG);
        phase.detail(finch::startup::PhaseDetail::category("debug_logging_probe"));
        if let Ok(temp_config) = load_config() {
            if temp_config.features.debug_logging {
                // Set RUST_LOG to debug if not already set by user
                if std::env::var("RUST_LOG").is_err() {
                    std::env::set_var("RUST_LOG", "debug");
                }
            }
        }
    }

    // NOW initialize tracing (will use the global OutputManager we just configured)
    {
        let _phase = finch::startup::phase(finch::startup::PHASE_TRACING);
        init_tracing();
    }

    // Load configuration (or run setup only when the file is genuinely
    // absent). A broken existing file is never an invitation to overwrite it
    // with auto-detected or default first-run state.
    let persisted_config = {
        let mut phase = finch::startup::phase(finch::startup::PHASE_CONFIG);
        phase.detail(finch::startup::PhaseDetail::category("persisted"));
        finch::config::load_persisted_config()
            .context("Existing Finch configuration could not be loaded and was left unchanged")?
    };
    let mut config = match persisted_config {
        Some(cfg) => cfg,
        None => match load_config() {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("{}", e);

                // Before showing the wizard, try to auto-detect API keys.
                // If any exist (env vars, Claude Code config, etc.) just start immediately.
                let auto_providers = build_cloud_providers_from_env();
                if !auto_providers.is_empty() {
                    let names: Vec<&str> = auto_providers
                        .iter()
                        .map(|entry| entry.provider_type())
                        .collect();
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
                    let cfg = Config::new(auto_providers);
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
                    finish_first_run_setup(
                        show_setup_wizard(),
                        |result| async move {
                            finch::cli::validate_first_run_and_apply(&result).await
                        },
                        finch::config::load_config,
                        &mut std::io::stderr(),
                    )
                    .await?
                } // end else (no auto-detected keys)
            }
        },
    };

    // Override TUI setting if --raw or --no-tui flag is provided
    if args.raw_mode || args.no_tui {
        config.tui_enabled = false;
        // Re-enable stdout for non-TUI modes
        output_manager.enable_stdout();
    }

    // --cloud-only: skip local model and daemon entirely
    if args.cloud_only {
        config.backend.enabled = false;
    }

    // Check for --direct or --cloud-only flags (both bypass daemon)
    // In direct/cloud-only mode: no daemon connection, talk directly to the cloud provider API
    let use_daemon = !args.direct && !args.cloud_only;

    // Load or create threshold router
    let _threshold_router_phase = finch::startup::phase(finch::startup::PHASE_THRESHOLD_ROUTER);
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

    drop(_threshold_router_phase);

    // Create router
    let router = Router::new(threshold_router);

    // Construct the named provider graph once. The compatibility client and daemon profile
    // router share these exact instances.
    // Built once here and, today, a second time inside `run_event_loop` via
    // `create_provider_profile_from_config`. Both are timed, so the report
    // shows the duplication rather than hiding it (#364).
    let claude_client = {
        let _phase = finch::startup::phase(finch::startup::PHASE_PROVIDER_GRAPH);
        let provider_graph = finch::providers::create_provider_graph_from_config(&config)?;
        ClaudeClient::with_shared_provider(provider_graph.default_provider())
    };

    // Create metrics logger
    let metrics_logger = {
        let _phase = finch::startup::phase(finch::startup::PHASE_METRICS);
        MetricsLogger::new(config.metrics_dir.clone())?
    };

    // Try to connect to daemon BEFORE creating Repl
    // This allows Repl to suppress local model logs if daemon is available
    use finch::client::{DaemonClient, DaemonConfig};
    let mut leftover_daemon_error = None;
    let daemon_client = if use_daemon && config.client.use_daemon {
        // The health probe behind this connect is what gates every launch:
        // GET /health, under a 500 ms client timeout whose expiry costs an
        // unconditional two-second sleep in
        // `ensure_daemon_running_after_isolation_gate` (#364).
        let mut phase = finch::startup::phase(finch::startup::PHASE_DAEMON_CONNECT);
        let daemon_config = DaemonConfig {
            bind_address: config.client.daemon_address.clone(),
            auto_spawn: config.client.auto_spawn,
            timeout_seconds: 5,
            api_key: config.server.api_keys.first().cloned(),
        };
        match DaemonClient::connect(daemon_config).await {
            Ok(client) => {
                phase.detail(finch::startup::PhaseDetail::category("connected"));
                Some(Arc::new(client))
            }
            Err(error) => {
                let message = error.to_string();
                let leftover = message.contains("finch daemon-stop");
                phase.detail(finch::startup::PhaseDetail::category(if leftover {
                    "incompatible"
                } else {
                    "unavailable"
                }));
                if leftover {
                    tracing::warn!("leftover daemon refused at health probe: {error}");
                    leftover_daemon_error = Some(message);
                } else {
                    tracing::debug!("Failed to connect to daemon: {error}");
                }
                None
            }
        }
    } else {
        finch::startup::phase(finch::startup::PHASE_DAEMON_CONNECT)
            .finish(finch::startup::PhaseDetail::category("disabled"));
        None
    };

    // Create and run REPL (with full TUI support)
    // Pass daemon_client so Repl knows whether to suppress local model logs
    // One name identifies the actual home Brain. Explicit names attach by name;
    // generated names include a short uniqueness suffix so a new console cannot
    // silently inherit an old Brain's memory and event history.
    let brain_name = resolve_brain_name(attach_brain, args.brain.clone())?;

    let mut repl = {
        let _phase = finch::startup::phase(finch::startup::PHASE_REPL_NEW);
        Repl::new(
            config,
            claude_client,
            router,
            metrics_logger,
            daemon_client,
            brain_name,
        )
        .await
    };
    repl.set_cli_selection(args.model.clone(), args.provider.clone());

    // Run REPL (with full TUI event loop)
    if std::env::var("SHAMMAH_DEBUG").is_ok() {
        eprintln!("[DEBUG] Starting REPL with full TUI...");
    }

    // Run inside a LocalSet so IpcClient (capnp-rpc, !Send) can use spawn_local.
    // Normal tokio::spawn calls inside the event loop still go to the thread pool.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Establish and version-check the IPC channel before the TUI
            // creates any Brain attachment or runner identity. Preserve a
            // failure for the startup projection instead of silently entering
            // a half-connected Brain state.
            {
                let mut phase = finch::startup::phase(finch::startup::PHASE_IPC_CONNECT);
                match finch::client::IpcClient::connect().await {
                    Ok(ipc) => {
                        if let Some(error) = leftover_daemon_error.take() {
                            phase.detail(finch::startup::PhaseDetail::category("incompatible"));
                            drop(ipc);
                            repl.set_daemon_ipc_error(error);
                        } else {
                            phase.detail(finch::startup::PhaseDetail::category("connected"));
                            repl.set_ipc_client(ipc);
                        }
                    }
                    Err(error) => {
                        let message = leftover_daemon_error
                            .take()
                            .unwrap_or_else(|| error.to_string());
                        phase.detail(finch::startup::PhaseDetail::category(
                            if message.contains("finch daemon-stop") {
                                "incompatible"
                            } else {
                                "unavailable"
                            },
                        ));
                        repl.set_daemon_ipc_error(message);
                    }
                }
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

    // #223: a per-process frontend diagnostic log, separate from any other
    // concurrently running frontend and from daemon.log, so the two can be
    // traced together after the fact. This is a plain file sink — it never
    // touches OutputManager and therefore never appears in the TUI
    // scrollback; it is debug/trace material for a person reading the file,
    // not conversation content.
    let frontend_file_layer = frontend_log_file().map(|log| {
        tracing_subscriber::fmt::layer()
            .with_writer(move || log.clone())
            .with_ansi(false)
    });

    // Build the subscriber with our custom layer
    tracing_subscriber::registry()
        .with(env_filter)
        .with(output_layer)
        .with(frontend_file_layer)
        .init();

    // Bridge log crate → tracing (for dependencies using log crate)
    // Do this after subscriber is set up
    tracing_log::LogTracer::init().ok();
}

/// Best-effort per-process frontend diagnostic log (#223). Never blocks or
/// fails interactive startup: a frontend that cannot create its log
/// directory or file still runs, just without this diagnostic sink.
fn frontend_log_file() -> Option<finch::daemon::RotatingLog> {
    let dir = finch::daemon::frontend_log_dir().ok()?;
    // Prune to one below the cap before creating this run's file. Pruning to
    // the cap itself and then opening a new file would retain cap + 1 forever.
    finch::daemon::prune_frontend_logs(
        &dir,
        finch::daemon::DEFAULT_MAX_FRONTEND_LOG_FILES.saturating_sub(1),
    );
    let identity = finch::daemon::frontend_log_identity();
    let path = finch::daemon::frontend_log_path(&identity).ok()?;
    match finch::daemon::RotatingLog::open(&path, finch::daemon::RotationPolicy::default()) {
        Ok(log) => {
            finch::cli::register_frontend_log_path(path.clone());
            if finch::cli::logging_enabled() {
                eprintln!("Frontend logs: {}", path.display());
            }
            Some(log)
        }
        Err(error) => {
            if finch::cli::logging_enabled() {
                eprintln!("Could not open frontend log file ({error:#}); continuing without it");
            }
            None
        }
    }
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
    use finch::daemon::{DaemonLifecycle, DaemonStopOutcome};

    let lifecycle = DaemonLifecycle::new()?;

    // Always call stop_daemon so crash leftovers are reaped even when
    // is_running is false.
    if lifecycle.is_running() {
        let pid = lifecycle.read_pid()?;
        println!("Stopping daemon (PID: {})...", pid);
    }

    match lifecycle.stop_daemon()? {
        DaemonStopOutcome::Stopped { .. } => {
            println!("✓ Daemon stopped successfully");
        }
        outcome => println!("{outcome}"),
    }
    Ok(())
}

/// Show daemon status
async fn run_daemon_status() -> Result<()> {
    use finch::daemon::DaemonLifecycle;

    let lifecycle = DaemonLifecycle::new()?;

    // Check if daemon is running
    if !lifecycle.is_running() {
        use crossterm::style::Stylize as _;
        if lifecycle.ipc_listener_alive() {
            // A live listener is not crash leftovers: daemon-stop refuses to
            // unlink a live socket, so suggesting it as cleanup would never
            // clear the warning.
            println!(
                "{}",
                "⚠ No daemon PID file, but the IPC socket still has a live listener"
                    .yellow()
                    .bold()
            );
            println!("  A process is still serving on the IPC socket; it may be a daemon");
            println!("  whose PID file was lost. `finch daemon-stop` will not remove a");
            println!("  live socket.");
            return Ok(());
        }
        println!("{}", "⚠ Daemon is not running".yellow().bold());
        if lifecycle.has_stale_files() {
            println!("  Leftover pid or socket files remain from a crashed process.");
            println!("  Clean them with: {}", "finch daemon-stop".cyan().bold());
        }
        println!("\nStart the daemon with:");
        println!("  {}", "finch daemon-start".cyan().bold());
        return Ok(());
    }

    // Get PID
    let pid = lifecycle.read_pid()?;

    // Query health endpoint
    let client = reqwest::Client::new();
    let daemon_url = format!("http://{}/health", finch::config::DEFAULT_DAEMON_ADDR);

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
        named_brains: usize,
        #[serde(default)]
        protocol_generation: u32,
        #[serde(default)]
        package_identity: String,
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
    println!("  Named Brains:    {}", health.named_brains);
    println!(
        "  Protocol:        {} ({})",
        health.protocol_generation,
        if health.package_identity.is_empty() {
            "unknown build"
        } else {
            health.package_identity.as_str()
        }
    );
    println!("  Bind Address:    {}", finch::config::DEFAULT_DAEMON_ADDR);
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
    println!(
        "Legacy experiment only: this installs Python, PyTorch, Transformers, and PEFT.\n\
         It does not enable daemon training or adapter loading. The manual script reads only the\n\
         JSONL path you pass it, may download/load a full base model, and can consume substantial\n\
         compute, memory, disk, and network bandwidth. Cancel a manual run by interrupting that\n\
         process. Input queues, the virtual environment, downloads, logs, and adapter outputs are\n\
         retained until you remove them; Finch will not process, migrate, or delete them.\n"
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
    println!(
        "\nAutomatic training remains disabled; explicit feedback is retained separately and does not run this script."
    );

    Ok(())
}

/// Awaits a spawned model-loader task's `JoinHandle` and, if the task
/// unwound from a panic (or was otherwise cancelled) rather than returning
/// normally, records that as `GeneratorState::Failed` instead of letting it
/// vanish silently.
///
/// `BootstrapLoader::load_generator_async` already turns a panic inside its
/// `spawn_blocking(...)` call into a normal `Err`, which the caller then
/// turns into `GeneratorState::Failed` -- but that is the only panic path it
/// catches. Before this helper, the task that runs `load_generator_async`
/// was spawned fire-and-forget: nothing awaited its `JoinHandle`, so a panic
/// anywhere else in that outer async block (e.g. in a `state.write().await`)
/// was logged by Tokio's default panic hook and then dropped, leaving
/// `state` stuck forever with no diagnostic signal to the user (#905).
async fn supervise_generator_loader_task(
    task: tokio::task::JoinHandle<()>,
    state: Arc<tokio::sync::RwLock<finch::models::GeneratorState>>,
) {
    if let Err(join_error) = task.await {
        let message = if join_error.is_panic() {
            format!("model loader task panicked: {join_error}")
        } else {
            format!("model loader task was cancelled: {join_error}")
        };
        tracing::error!("{}", message);
        *state.write().await = finch::models::GeneratorState::Failed { error: message };
    }
}

async fn run_daemon(bind_address: String) -> Result<()> {
    use finch::daemon::DaemonLifecycle;
    use finch::local::LocalGenerator;
    use finch::models::{BootstrapLoader, GeneratorState};
    use finch::server::{AgentServer, ServerConfig};
    use finch::{output_progress, output_status};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // An isolated daemon must authenticate the supervisor before logging,
    // loading config, probing lifecycle state, or creating any Finch files.
    // Runs before tracing is initialized below, so timing goes to stderr
    // (captured by the integration-test harness) rather than daemon.log.
    // This is the first of several call sites that validate the supervisor
    // proof over a daemon's startup (#858); the validation itself is cached
    // for the process, so only whichever call site runs first pays the cost
    // of reading and hashing the supervisor executable.
    //
    // The pre-validation stderr marker names the silent phase. #868's CI
    // signature was an alive process with a 0-byte log and no bind: the
    // 10s/30s-bound runs caught the daemon here, in this hash, before the log
    // file existed, and the 60s run stalled one call site later, in the same
    // validation inside `AgentServer::new` — under runner load each hash
    // costs tens of seconds. The stall is now attributable from the captured
    // stderr alone. stderr is this function's established pre-auth
    // diagnostic channel (the timing line below), and the marker carries no
    // authority material.
    eprintln!(
        "[daemon] validating supervisor authority before startup (pid={})...",
        std::process::id()
    );
    let proof_start = std::time::Instant::now();
    let isolated_proof = finch::brain::isolated_test_proof_if_present()?;
    eprintln!(
        "isolated_test_proof_if_present (run_daemon entry): {}ms, present={}",
        proof_start.elapsed().as_millis(),
        isolated_proof.is_some()
    );
    let bind_address = if let Some(proof) = &isolated_proof {
        anyhow::ensure!(
            bind_address == proof.daemon_address(),
            "isolated daemon CLI address does not match supervisor authority"
        );
        proof.daemon_address().to_owned()
    } else {
        bind_address
    };

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

    // Set up bounded file logging for daemon (rotating ~/.finch/daemon.log)
    let log_path = finch::daemon::daemon_log_path()?;
    let policy = finch::daemon::RotationPolicy::from_env();
    let rotating_log = finch::daemon::RotatingLog::open(&log_path, policy)?;
    let log_status = rotating_log.status();

    // Own this process's descriptors so output that never passes through
    // tracing — println!, panic output, ONNX Runtime's C++ stderr — follows
    // rotation instead of pinning the inode inherited from the frontend (#249).
    //
    // Only the detached child does this. `finch daemon` run in a terminal and
    // `finch worker` are documented foreground modes, and the shipped systemd
    // unit runs `finch daemon` expecting its output in the journal; binding
    // unconditionally would send all three — including startup failures and
    // panics — into a file and leave the operator with a blank terminal.
    #[cfg(unix)]
    if std::env::var_os(finch::daemon::DETACHED_DAEMON_ENV).is_some() {
        rotating_log.bind_process_stdio()?;
        // Consume the marker. It lives in this process's environment and would
        // otherwise be inherited by every child the daemon spawns — bash tools,
        // MCP stdio servers, the upgrade preflight probe — each of which would
        // then hijack its own descriptors and discard the diagnostics its
        // caller is collecting.
        std::env::remove_var(finch::daemon::DETACHED_DAEMON_ENV);
    }

    // Create a file logger layer over the rotating writer
    let file_writer = rotating_log.clone();
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

    eprintln!("Daemon logs: {}", log_status.summary());

    // Suppress ONNX Runtime verbose logs (must be set before library
    // initialization), unless the operator asked for more (#223 diagnostics).
    suppress_ort_logs_unless_overridden();

    // Note: init_tracing() is NOT called in daemon mode - we set up file logging above instead

    tracing::info!("Starting Shammah in daemon mode");

    // Initialize daemon lifecycle (PID file management)
    let lifecycle = DaemonLifecycle::new()?;

    // The process-lifetime lock is authoritative. A PID file alone has a
    // check-then-write race and allowed two coordinators to unlink/rebind the
    // same Unix socket.
    let daemon_instance = lifecycle.acquire_instance()?;
    tracing::info!(pid = std::process::id(), "Daemon PID file written");

    // Load configuration
    let mut config = load_config()?;
    config.server.enabled = true;
    config.server.bind_address = bind_address.clone();
    if let Some(proof) = &isolated_proof {
        config.server.brain_password = proof.brain_password()?;
        config.server.advertise = false;
    }

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

    // Construct once, then share the same provider instances between compatibility forwarding
    // and named daemon profile routing.
    let provider_graph = finch::providers::create_provider_graph_from_config(&config)?;
    let claude_client = ClaudeClient::with_shared_provider(provider_graph.default_provider());

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Initialize BootstrapLoader for progressive Qwen model loading
    output_progress!("⏳ Initializing Qwen model (background)...");
    let generator_state = Arc::new(RwLock::new(GeneratorState::Initializing));
    let model_progress: Arc<dyn finch::models::ModelProgress> = finch::cli::global_output();
    let bootstrap_loader = Arc::new(
        BootstrapLoader::new(Arc::clone(&generator_state), Some(model_progress))
            .with_huggingface_token(config.huggingface_token.clone()),
    );

    // Start background model loading (unless backend is disabled for proxy-only mode)
    if config.backend.enabled {
        let loader_clone = Arc::clone(&bootstrap_loader);
        let state_clone = Arc::clone(&generator_state);
        let provider = config.backend.inference_provider;
        let model_family = config.backend.model_family;
        let model_size = config.backend.model_size;
        let device = config.backend.execution_target;
        let model_path = config.backend.model_path.clone();
        let managed_artifact = config.backend.managed_artifact.clone();
        let loader_task = tokio::spawn(async move {
            if let Err(e) = loader_clone
                .load_generator_async(
                    provider,
                    model_family,
                    model_size,
                    device,
                    model_path,
                    managed_artifact,
                )
                .await
            {
                output_status!("⚠️  Model loading failed: {}", e);
                output_status!("   Will forward all queries to cloud provider APIs");
                let mut state = state_clone.write().await;
                *state = GeneratorState::Failed {
                    error: format!("{}", e),
                };
            }
        });
        // The task above already turns an `Err` from `load_generator_async`
        // into `GeneratorState::Failed` -- but the inner `spawn_blocking(...)`
        // panic path (BootstrapLoader::load_generator_async) is the *only*
        // panic that call chain catches. A panic anywhere else in the outer
        // async block (e.g. a `state_clone.write().await` or similar) would
        // otherwise kill this task silently: Tokio's default panic hook logs
        // it and nothing awaits the JoinHandle to notice, leaving
        // `generator_state` stuck forever and the poller below spinning with
        // no diagnostic signal to the user (#905). Supervise the handle so
        // that outcome surfaces the same way the guarded inner panic does.
        tokio::spawn(supervise_generator_loader_task(
            loader_task,
            Arc::clone(&generator_state),
        ));
    } else {
        // Proxy-only mode: Skip model loading
        output_status!("🔌 Proxy-only mode enabled (no local model)");
        output_status!("   All queries will be forwarded to cloud provider APIs");
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

    output_status!("LoRA training disabled (native training support is not available)");

    // Create server configuration
    let server_config = ServerConfig {
        bind_address: config.server.bind_address.clone(),
        brain_bind_address: config
            .server
            .advertise
            .then(|| config.server.brain_bind_address.clone()),
        auth_enabled: config.server.auth_enabled,
        api_keys: config.server.api_keys.clone(),
        brain_password: config.server.brain_password.clone(),
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
        provider_graph,
    )?;
    // #868: the construction above is a long silent stretch between the last
    // generator log line and the serve path's first log line — supervisor
    // proof revalidation, credential authority, Brain store, feedback store —
    // so a stall in it looked like "alive, 0-byte log, never binds". The
    // failing 60s CI run stopped exactly here: its last log line was the
    // generator's, so the blocked phase was this construction. Name the
    // boundary and how long it took.
    tracing::info!(
        elapsed_ms = proof_start.elapsed().as_millis(),
        "daemon startup: agent server constructed"
    );

    // Set up mDNS service advertisement if enabled
    let service_discovery = if config.server.advertise {
        use finch::service::{ServiceConfig, ServiceDiscovery};

        let service_config = ServiceConfig {
            name: config.server.service_name.clone(),
            description: config.server.service_description.clone(),
            node_public_key: server.brain_credentials().invitation_public_key(),
        };

        match ServiceDiscovery::new(service_config) {
            Ok(discovery) => {
                // Advertise the encrypted collaboration listener, never the
                // loopback/plain daemon administration listener.
                let port = config
                    .server
                    .brain_bind_address
                    .split(':')
                    .next_back()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(finch::config::DEFAULT_BRAIN_TLS_PORT);

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
    let mut server_handle = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.serve().await }
    });

    // Start Cap'n Proto IPC server on Unix socket (internal CLI ↔ daemon channel).
    // capnp-rpc uses !Send futures, so we run it on a dedicated single-threaded runtime
    // inside a spawn_blocking thread rather than tokio::spawn (which requires Send).
    let ipc_shutdown = tokio_util::sync::CancellationToken::new();
    let mut ipc_handle = tokio::task::spawn_blocking({
        let server = Arc::clone(&server);
        let shutdown = ipc_shutdown.clone();
        move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("IPC tokio runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(finch::server::start_ipc_server(server, shutdown)))
        }
    });

    // Wait for shutdown signal (Ctrl+C or SIGTERM)
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT, shutting down gracefully");
        }
        result = &mut server_handle => {
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
        result = &mut ipc_handle => {
            match result {
                Ok(Ok(())) => tracing::info!("IPC server exited normally"),
                Ok(Err(e)) => tracing::error!(error = %e, "IPC server error"),
                Err(e) => tracing::error!(error = %e, "IPC server task panicked"),
            }
        }
    }

    // Stop an in-flight managed transfer before tearing down the runtimes. The
    // verified final file is never published on cancellation; its partial is
    // retained for the next daemon start to resume.
    server.bootstrap_loader().cancel_download();

    // The IPC accept loop runs in a dedicated current-thread runtime. It must
    // observe cancellation and finish before the PID/instance lock is released;
    // dropping a spawn_blocking JoinHandle does not stop its thread.
    ipc_shutdown.cancel();
    if !ipc_handle.is_finished() {
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), &mut ipc_handle)
            .await
            .context("IPC server did not stop within two seconds")??;
        stopped?;
    }
    if !server_handle.is_finished() {
        server_handle.abort();
    }

    // Stop mDNS advertisement if enabled
    if let Some(discovery) = service_discovery {
        if let Err(e) = discovery.stop() {
            tracing::warn!("Failed to stop service advertisement: {}", e);
        }
    }

    // Remove only this process's PID metadata, then release exclusive daemon
    // ownership. Drop performs the same owner-safe cleanup on early returns.
    daemon_instance.release()?;
    tracing::info!("Daemon shutdown complete");

    Ok(())
}

fn query_tool_state_paths(home: Option<PathBuf>) -> Result<(PathBuf, PathBuf)> {
    let state_root = home
        .context("Could not determine an application-state root for query tools")?
        .join(".finch");
    Ok((
        state_root.join("tool_patterns.json"),
        state_root.join("source-index"),
    ))
}

/// Build the standard tool registry + executor used for non-interactive query mode.
/// Auto-approves all tools (no interactive prompting in non-interactive mode).
async fn build_query_tool_executor(
    config: &Config,
) -> Result<(
    Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    Vec<finch::tools::ToolDefinition>,
    Arc<finch::runtime::ProgramRuntime>,
)> {
    use finch::tools::{
        BashTool, CodeOutlineTool, EditTool, FindCodeTool, GlobTool, GrepTool, PatchTool, ReadTool,
        WebFetchTool, WriteTool,
    };
    use finch::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};

    let mut registry = ToolRegistry::new();
    let tool_workspace_root = finch::tools::resolve_workspace_root(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );
    let (patterns_path, source_index_state) = query_tool_state_paths(dirs::home_dir())?;
    registry.register(Box::new(ReadTool));
    registry.register(Box::new(GlobTool));
    registry.register(Box::new(GrepTool));
    registry.register(Box::new(CodeOutlineTool::new(tool_workspace_root.clone())));
    registry.register(Box::new(FindCodeTool::new(
        tool_workspace_root.clone(),
        source_index_state,
    )));
    registry.register(Box::new(WebFetchTool::new()));
    registry.register(Box::new(BashTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(PatchTool));
    registry.register(Box::new(WriteTool));

    let program_runtime = Arc::new(finch::runtime::ProgramRuntime::new());
    register_query_vm_tools(&mut registry, Arc::clone(&program_runtime));

    // Auto-approve everything in non-interactive mode
    let permissions = PermissionManager::new()
        .with_workspace_root(tool_workspace_root)
        .with_default_rule(PermissionRule::Allow);
    let executor = ToolExecutor::new(registry, permissions, patterns_path)
        .context("Failed to create tool executor")?
        .with_mcp(config)
        .await
        // Declared post-edit diagnostics sources (issue #757). Inert without
        // a [diagnostics] declaration.
        .with_diagnostics(&config.diagnostics);
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
    use finch::tools::{
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
    // An apostrophe is a contraction, and no Co-Forth word contains one. This has to be tested
    // before the operator characters below, not after: `don't!` contains `!`, so without this the
    // operator test claims it as a program and the typed runtime fails on an unknown word (#571).
    if t.contains('\'') {
        return false;
    }
    if t.starts_with(|c: char| c.is_uppercase()) {
        return false;
    }
    // Token-boundary `@path` mentions are query attachments, not Forth fetch.
    // A trailing bare `@` (`dup @`, `5 @`) is not a mention and stays Forth.
    if !finch::context::parse_visible_mentions(t).is_empty() {
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
async fn run_query(query: &str, cloud_only: bool, show_program: bool) -> Result<()> {
    use finch::client::DaemonClient;
    use finch::daemon::ensure_daemon_running;

    // Short-circuit: typed Lisp expressions start with `(` — before Forth check.
    if query.trim_start().starts_with('(') {
        println!("{}", query);
        run_direct_typed_source(finch_programs::ProgramLanguage::Lisp, query).await?;
        return Ok(());
    }

    // Short-circuit: run typed Co-Forth directly, no AI involved.
    if is_clearly_forth(query) {
        println!("{}", query);
        run_direct_typed_source(finch_programs::ProgramLanguage::Forth, query).await?;
        return Ok(());
    }

    let query = match finch::context::prepare_prompt_for_query(
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        query,
    ) {
        Ok((prepared, _)) => prepared,
        Err(diagnostic) => {
            anyhow::bail!("{diagnostic}");
        }
    };
    let query = query.as_str();

    // Load configuration
    let config = load_config()?;

    // Build tool executor (same tools as the REPL)
    let (executor, tool_definitions, program_runtime) = build_query_tool_executor(&config).await?;

    // A one-shot cloud-only query must not first attempt the daemon. Besides
    // defeating the flag, that startup attempt can consume the whole caller
    // timeout and makes direct-provider smoke tests look hung.
    if cloud_only {
        return run_query_cloud_only(
            query,
            &config,
            executor,
            tool_definitions,
            program_runtime,
            show_program,
        )
        .await;
    }

    // Ensure daemon is running (auto-spawn if needed)
    if let Err(e) = ensure_daemon_running(Some(&config.client.daemon_address)).await {
        eprintln!("⚠️  Daemon failed to start: {}", e);
        eprintln!("   Using the cloud provider API directly (no local model)");
        return run_query_cloud_only(
            query,
            &config,
            executor,
            tool_definitions,
            program_runtime,
            show_program,
        )
        .await;
    }

    // Create daemon client and run full tool loop
    let daemon_config = finch::client::DaemonConfig::from_client_config(&config.client);
    let client = DaemonClient::connect(daemon_config).await?;

    let guard = executor.lock().await;
    let wire_metrics = default_wire_metrics_logger();
    let mut wire_metric =
        finch::metrics::WireAdherenceMetric::first_pass("daemon", "daemon-selected", "one_shot");
    let response = client
        .query_with_tools_with_system(
            query,
            Some(vm_wire_system_prompt()),
            tool_definitions,
            &guard,
        )
        .await?;
    finch_programs::capture_with_compiler_context_from_env(
        || program_runtime.compiler_context(),
        "daemon",
        "daemon-selected",
        "one_shot",
        finch_programs::WireCorpusAttempt::FirstPass,
        &response,
    );
    if show_program {
        print_wire_program(&response);
    }
    // The daemon owns model inference and tool-loop routing, but this CLI
    // process owns the local typed runtime.  A final text response is therefore
    // still Finch wire source, never user-facing prose to print verbatim.
    // Running it here keeps the daemon and --cloud-only paths semantically
    // identical without handing workspace/UI authority to the daemon.
    let outcome = match execute_one_shot_wire_source(&program_runtime, &response).await {
        Ok(outcome) if outcome.status == finch::runtime::ExecutionStatus::Completed => {
            finish_wire_metric(
                wire_metrics.as_ref(),
                &mut wire_metric,
                Some(&outcome.output),
                false,
            );
            print!("{}", outcome.output);
            return Ok(());
        }
        Ok(outcome) if can_repair_one_shot_wire_outcome(&outcome) => {
            let diagnostic = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            if show_program {
                eprintln!("→ rejected: {diagnostic}");
            }
            mark_wire_rejection(&mut wire_metric, &response, &diagnostic);
            wire_metric.repair_attempted = true;
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
            finch_programs::capture_with_compiler_context_from_env(
                || program_runtime.compiler_context(),
                "daemon",
                "daemon-selected",
                "one_shot",
                finch_programs::WireCorpusAttempt::Repair,
                &repair,
            );
            if show_program {
                print_wire_program(&repair);
            }
            execute_one_shot_wire_source(&program_runtime, &repair).await?
        }
        Ok(outcome) => outcome,
        Err(error) if is_repairable_one_shot_wire_diagnostic(&error.to_string()) => {
            if show_program {
                eprintln!("→ rejected: {error}");
            }
            mark_wire_rejection(&mut wire_metric, &response, &error.to_string());
            wire_metric.repair_attempted = true;
            let repair = client
                .query_with_tools_with_system(
                    &one_shot_wire_repair_request(&response, &error.to_string()),
                    Some(vm_wire_system_prompt()),
                    Vec::new(),
                    &guard,
                )
                .await?;
            finch_programs::capture_with_compiler_context_from_env(
                || program_runtime.compiler_context(),
                "daemon",
                "daemon-selected",
                "one_shot",
                finch_programs::WireCorpusAttempt::Repair,
                &repair,
            );
            if show_program {
                print_wire_program(&repair);
            }
            execute_one_shot_wire_source(&program_runtime, &repair).await?
        }
        Err(error) => {
            mark_wire_rejection(&mut wire_metric, &response, &error.to_string());
            finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
            return Err(error);
        }
    };
    if outcome.status == finch::runtime::ExecutionStatus::Completed {
        finish_wire_metric(
            wire_metrics.as_ref(),
            &mut wire_metric,
            Some(&outcome.output),
            false,
        );
        print!("{}", outcome.output);
    } else {
        let diagnostic = outcome
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
        if wire_metric.first_pass_valid {
            mark_wire_rejection(&mut wire_metric, &response, &diagnostic);
        }
        finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
        anyhow::bail!(
            "daemon provider VM-wire program ended as {:?}: {}",
            outcome.status,
            diagnostic
        );
    }

    Ok(())
}

/// Run query using the cloud provider API only (fallback when daemon fails), with tool support
async fn run_query_cloud_only(
    query: &str,
    config: &Config,
    executor: Arc<tokio::sync::Mutex<finch::tools::ToolExecutor>>,
    tool_definitions: Vec<finch::tools::ToolDefinition>,
    program_runtime: Arc<finch::runtime::ProgramRuntime>,
    show_program: bool,
) -> Result<()> {
    use finch::claude::MessageRequest;
    use finch::providers::{ContentBlock, Message};

    eprintln!("⚠️  Running in cloud-only mode (no local model)");

    let claude_client = create_claude_client_with_provider(config)?;
    let model = config
        .cloud_providers()
        .first()
        .and_then(|provider| provider.model().map(str::to_string))
        .unwrap_or_else(|| finch::config::DEFAULT_CLAUDE_MODEL.to_string());
    let provider = config
        .cloud_providers()
        .first()
        .map(|provider| provider.provider_type().to_string())
        .unwrap_or_else(|| "cloud".to_string());
    let wire_metrics = default_wire_metrics_logger();
    let mut wire_metric =
        finch::metrics::WireAdherenceMetric::first_pass(&provider, model.clone(), "one_shot");

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
            max_tokens: finch::config::DEFAULT_MAX_TOKENS,
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
            finch_programs::capture_with_compiler_context_from_env(
                || program_runtime.compiler_context(),
                &provider,
                &model,
                "one_shot",
                if wire_repair_requested {
                    finch_programs::WireCorpusAttempt::Repair
                } else {
                    finch_programs::WireCorpusAttempt::FirstPass
                },
                &source,
            );
            if show_program {
                print_wire_program(&source);
            }
            let outcome = match execute_one_shot_wire_source(&program_runtime, &source).await {
                Ok(outcome) => outcome,
                Err(error)
                    if !wire_repair_requested
                        && is_repairable_one_shot_wire_diagnostic(&error.to_string()) =>
                {
                    if show_program {
                        eprintln!("→ rejected: {error}");
                    }
                    mark_wire_rejection(&mut wire_metric, &source, &error.to_string());
                    wire_metric.repair_attempted = true;
                    messages.push(response.to_message());
                    messages.push(Message::user(one_shot_wire_repair_request(
                        &source,
                        &error.to_string(),
                    )));
                    wire_repair_requested = true;
                    continue;
                }
                Err(error) => {
                    if wire_metric.first_pass_valid {
                        mark_wire_rejection(&mut wire_metric, &source, &error.to_string());
                    }
                    finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
                    return Err(error);
                }
            };
            if outcome.status == finch::runtime::ExecutionStatus::Completed {
                finish_wire_metric(
                    wire_metrics.as_ref(),
                    &mut wire_metric,
                    Some(&outcome.output),
                    false,
                );
                print!("{}", outcome.output);
                return Ok(());
            }

            let diagnostic = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            if !wire_repair_requested && can_repair_one_shot_wire_outcome(&outcome) {
                if show_program {
                    eprintln!("→ rejected: {diagnostic}");
                }
                mark_wire_rejection(&mut wire_metric, &source, &diagnostic);
                wire_metric.repair_attempted = true;
                messages.push(response.to_message());
                messages.push(Message::user(one_shot_wire_repair_request(
                    &source,
                    &diagnostic,
                )));
                wire_repair_requested = true;
                continue;
            }
            if wire_metric.first_pass_valid {
                mark_wire_rejection(&mut wire_metric, &source, &diagnostic);
            }
            finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
            anyhow::bail!(
                "provider VM-wire program ended as {:?}: {}",
                outcome.status,
                diagnostic
            );
        }

        if wire_repair_requested {
            finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
            anyhow::bail!("provider used tools while repairing a rejected Finch VM wire program");
        }

        // Execute tool calls and collect results
        messages.push(response.to_message());

        let tool_uses = response.tool_uses();
        let mut result_blocks = Vec::new();
        for tu in &tool_uses {
            let tool_use = finch::tools::ToolUse {
                id: tu.id.clone(),
                name: tu.name.clone(),
                input: tu.input.clone(),
            };
            let exec_result = {
                let guard = executor.lock().await;
                guard
                    .execute_tool::<fn() -> anyhow::Result<()>>(
                        &tool_use, None, // save_models_fn
                        None, // repl_mode
                        None, // plan_content
                        None, // live_output
                        None, // effect_audit
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

    if wire_metric.repair_attempted {
        finish_wire_metric(wire_metrics.as_ref(), &mut wire_metric, None, true);
    }
    eprintln!("⚠️  Reached max tool turns without a final answer");
    Ok(())
}

/// Render a raw model response for an explicit human inspection request.
///
/// This stays on stderr so `finch query` retains stdout as the executed
/// program's user-visible result, which keeps scripts and shell pipelines
/// stable. The source is never executed by this helper.
fn print_wire_program(source: &str) {
    match finch_programs::ProgramLanguage::infer_wire_source(source) {
        Ok(language) => eprintln!("→ program ({})\n{}", language.as_str(), source),
        Err(error) => eprintln!("→ program (invalid wire source: {error})\n{source}"),
    }
}

/// The same provider-facing contract accompanies every one-shot transport.
/// Keep it out of an ordinary user message so a user request cannot be
/// confused with the wire protocol itself.
fn vm_wire_system_prompt() -> String {
    const VM_WIRE_BOOT: &str = include_str!("../vocabulary/BOOT.md");
    format!(
        "{}\n\n{}",
        finch::generators::CODING_SYSTEM_PROMPT,
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
) -> Result<finch::runtime::ExecutionOutcome> {
    let language = finch_programs::ProgramLanguage::infer_wire_source(source)?;
    program_runtime
        .submit_typed_only(finch::runtime::ProgramSubmission {
            language,
            source_id: Some(format!("provider-response.{}", language.as_str())),
            source: source.to_string(),
            intent: "one-shot provider VM-wire response".to_string(),
            effect: finch_programs::ExecutionEffect::Pure,
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
fn can_repair_one_shot_wire_outcome(outcome: &finch::runtime::ExecutionOutcome) -> bool {
    use finch::runtime::ExecutionStatus;

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
    finch_programs::is_repairable_wire_diagnostic(diagnostic)
}

fn one_shot_wire_repair_request(rejected_source: &str, diagnostic: &str) -> String {
    finch_programs::wire_repair_request(rejected_source, diagnostic)
}

fn default_wire_metrics_logger() -> Option<finch::metrics::MetricsLogger> {
    dirs::home_dir()
        .map(|home| home.join(".finch").join("metrics"))
        .and_then(|path| finch::metrics::MetricsLogger::new(path).ok())
}

fn mark_wire_rejection(
    metric: &mut finch::metrics::WireAdherenceMetric,
    source: &str,
    diagnostic: &str,
) {
    metric.first_pass_valid = false;
    metric.failure_class = Some(finch_programs::classify_wire_failure(source, diagnostic));
    metric.diagnostic_code = finch_programs::wire_diagnostic_code(diagnostic);
}

fn finish_wire_metric(
    logger: Option<&finch::metrics::MetricsLogger>,
    metric: &mut finch::metrics::WireAdherenceMetric,
    output: Option<&str>,
    terminal_failure: bool,
) {
    if output.is_some_and(str::is_empty) {
        metric.first_pass_valid = false;
        metric.failure_class = Some(finch::metrics::WireFailureClass::MissingOutputEffect);
    }
    metric.repaired_successfully = metric.repair_attempted
        && !terminal_failure
        && output.is_some_and(|value| !value.is_empty());
    metric.terminal_failure = terminal_failure || output.is_some_and(str::is_empty);
    if let Some(logger) = logger {
        if let Err(error) = logger.log_wire(metric) {
            tracing::warn!("failed to record provider wire adherence: {error}");
        }
    }
}

/// Run interactive setup wizard
async fn run_setup() -> Result<()> {
    use finch::cli::show_setup_wizard;

    println!("Starting Shammah setup wizard...\n");

    // Run the wizard
    let result = show_setup_wizard()?;
    if finch::cli::validate_command_and_apply(&result).await?
        == finch::cli::SetupApplyOutcome::Cancelled
    {
        println!("Setup cancelled; configuration was not changed.");
        return Ok(());
    }

    println!("\n✓ Configuration saved to ~/.finch/config.toml");
    println!("  You can now run: finch");
    println!("  Or start the daemon: finch daemon\n");

    Ok(())
}

fn auth_provider(command: &AuthCommand) -> &str {
    match command {
        AuthCommand::Status { provider, .. }
        | AuthCommand::Login { provider, .. }
        | AuthCommand::Logout { provider, .. }
        | AuthCommand::Recover { provider, .. } => provider,
    }
}

async fn run_auth_command(command: AuthCommand) -> Result<()> {
    match auth_provider(&command) {
        "chatgpt" => run_chatgpt_auth(command).await,
        "grok-sub" => run_grok_auth(command).await,
        other => anyhow::bail!("unsupported auth provider {other}"),
    }
}

async fn run_chatgpt_auth(command: AuthCommand) -> Result<()> {
    use finch::cli::{
        render_chatgpt_auth_status_line, save_chatgpt_named_credential, ChatGptAuthService,
        ChatGptDeviceLoginPresentation,
    };

    let service = ChatGptAuthService::production()?;
    match command {
        AuthCommand::Status { credential, .. } => {
            let status = service.status(&credential)?;
            println!("{}", render_chatgpt_auth_status_line(&status)?);
        }
        AuthCommand::Login {
            credential,
            copy,
            open,
            ..
        } => {
            let cancel = command_cancellation();
            let metadata = service
                .login(
                    &credential,
                    ChatGptDeviceLoginPresentation {
                        copy_code: copy,
                        open_browser: open,
                    },
                    cancel,
                )
                .await?;
            let account = metadata.account.clone().unwrap_or_default();
            let config = load_config().context(
                "ChatGPT login succeeded, but Finch config is unavailable; rerun `finch setup` to bind the named credential",
            )?;
            save_chatgpt_named_credential(config, metadata)?;
            println!("ChatGPT login saved credential {credential} for account {account}.");
        }
        AuthCommand::Logout { credential, .. } => {
            let metadata = service.logout(&credential, command_cancellation()).await?;
            let config = load_config()?;
            save_chatgpt_named_credential(config, metadata)?;
            println!("ChatGPT credential {credential} was revoked and signed out.");
        }
        AuthCommand::Recover { credential, .. } => {
            let metadata = service.recover(&credential)?;
            let config = load_config()?;
            save_chatgpt_named_credential(config, metadata)?;
            println!("Recovered ChatGPT credential {credential} as signed_out; run `finch auth login chatgpt --credential {credential}` to sign in again.");
        }
    }
    Ok(())
}

async fn run_grok_auth(command: AuthCommand) -> Result<()> {
    use finch::cli::{
        render_grok_auth_status_line, save_grok_named_credential, GrokAuthService,
        GrokDeviceLoginPresentation,
    };

    let service = GrokAuthService::production()?;
    match command {
        AuthCommand::Status { credential, .. } => {
            let status = service.status(&credential)?;
            println!("{}", render_grok_auth_status_line(&status)?);
        }
        AuthCommand::Login {
            credential,
            copy,
            open,
            ..
        } => {
            let cancel = command_cancellation();
            let metadata = service
                .login(
                    &credential,
                    GrokDeviceLoginPresentation {
                        copy_code: copy,
                        open_browser: open,
                    },
                    cancel,
                )
                .await?;
            let account = metadata.account.clone().unwrap_or_default();
            let config = load_config().context(
                "Grok login succeeded, but Finch config is unavailable; rerun `finch setup` to bind the named credential",
            )?;
            save_grok_named_credential(config, metadata)?;
            println!("Grok login saved credential {credential} for account {account}.");
        }
        AuthCommand::Logout { credential, .. } => {
            let metadata = service.logout(&credential, command_cancellation()).await?;
            let config = load_config()?;
            save_grok_named_credential(config, metadata)?;
            println!("Grok credential {credential} was revoked and signed out.");
        }
        AuthCommand::Recover { credential, .. } => {
            let metadata = service.recover(&credential)?;
            let config = load_config()?;
            save_grok_named_credential(config, metadata)?;
            println!("Recovered Grok credential {credential} as signed_out; run `finch auth login grok-sub --credential {credential}` to sign in again.");
        }
    }
    Ok(())
}

fn command_cancellation() -> tokio_util::sync::CancellationToken {
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    cancel
}

fn current_node_capabilities(has_cloud_provider: bool) -> finch::node::NodeCapabilities {
    use finch::models::{ModelSelection, ModelSelector};

    let ram_gb = ModelSelector::get_total_ram_gb();
    let local_model = match ModelSelector::select_for_system() {
        Ok(ModelSelection::Local(size)) => Some(size.description().to_string()),
        _ => None,
    };
    finch::node::NodeCapabilities::for_current_host(ram_gb, local_model, has_cloud_provider)
}

/// Show this node's identity and capabilities
async fn run_node_info() -> Result<()> {
    use finch::node::NodeInfo;

    let config = load_config().unwrap_or_else(|_| Config::new(vec![]));
    let has_cloud_provider = !config.cloud_providers().is_empty();
    let info = NodeInfo::load(current_node_capabilities(has_cloud_provider))?;

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
        println!("  Model    : cloud-only (cloud provider API)");
    }
    println!(
        "  Cloud    : {}",
        if info.capabilities.has_cloud_provider {
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

fn run_wire_corpus_command(cmd: WireCorpusCommand) -> Result<()> {
    match cmd {
        WireCorpusCommand::Audit { corpus, json } => {
            let report = finch_programs::audit(&corpus)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            println!("Finch provider wire corpus audit");
            println!("  report only: source was compiled and verified, never executed");
            println!("  total:       {}", report.counts.total);
            println!("  accepted:    {}", report.counts.accepted);
            println!("  rejected:    {}", report.counts.rejected);
            println!("  Lisp:        {}", report.counts.lisp);
            println!("  Co-Forth:    {}", report.counts.forth);
            for (versions, count) in &report.source_versions {
                println!("  source {versions}: {count}");
            }
            for (class, count) in &report.counts.failure_classes {
                println!("  failure {class}: {count}");
            }
            for (code, count) in &report.counts.diagnostics {
                println!("  diagnostic {code}: {count}");
            }
            for (provider_model, counts) in report.by_provider_model {
                println!(
                    "  {provider_model}: {} total, {} accepted, {} rejected",
                    counts.total, counts.accepted, counts.rejected
                );
            }
        }
    }
    Ok(())
}

/// Handle `finch network` subcommands
async fn run_network_command(cmd: NetworkCommand) -> Result<()> {
    use finch::network::client::RegisterDeviceRequest;
    use finch::network::{DeviceMembership, LotusClient, MembershipStatus};
    use finch::node::NodeIdentity;

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
    let has_cloud_provider = !config.cloud_providers().is_empty();
    let info = NodeInfo::load(current_node_capabilities(has_cloud_provider))?;

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
        println!("  Model    : cloud-only — forwarding to the cloud provider API");
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
            // Removing a licence un-suppressed the notice as a side effect of
            // writing `notice_suppress_until: None`. The record lives in a
            // state file now, so that has to be explicit (#329 review).
            finch::config::forget_notice_suppression();
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

    // Load config (needs a cloud provider for the agentic loop)
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {}", e);
            eprintln!("Run `finch setup` to configure a cloud provider API key.");
            return Err(e);
        }
    };

    if config.cloud_providers().is_empty() {
        anyhow::bail!(
            "No cloud provider configured.\n\
             Agent mode requires a cloud provider API (Claude, GPT-4, etc.).\n\
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
    // `path` is `./**`: a normalized relative path inside the workspace root.
    // An absolute path under ~/.finch is rejected by the type checker before it
    // reaches the broker, so the instruction has to start with a copy. The
    // previous version of these lines advertised `xlsx@`, a word of the
    // Co-Forth interpreter that no user input could reach and that #294
    // removed -- and the first replacement for them named `Sheet1`, passed
    // `workbook-summary` two of its three arguments, and interpolated this
    // absolute path. Printing an instruction nobody ran is how both happened,
    // so `run_samples_instructions_name_real_sheets_and_arities` now pins these
    // strings against the vocabulary and the generated sheet names.
    println!("Copy one into your workspace, then type in the REPL:");
    println!("  cp {}/grades.xlsx .", dir.display());
    for line in sample_repl_instructions() {
        println!("  {line}");
    }
    Ok(())
}

/// The `finch samples` REPL instructions, as data so a test can compile them.
///
/// No `/lisp` prefix: there is no such command. `Command::parse`'s catch-all
/// turns any unrecognised `/word` into `Command::Help`, so the first version of
/// these lines printed the help screen. The REPL routes a bare leading `(` to
/// the typed Lisp path -- `src/cli/repl_event/event_loop.rs` states the rule --
/// so that is what these are.
///
/// They reach the capability broker and stop there, awaiting approval, which is
/// the interactive workflow. `run_samples_instructions_compile_and_reach_the_broker`
/// submits each through the real runtime, so a wrong word, arity, argument
/// order, argument type or path form fails as a link, type or path error before
/// approval is ever asked for.
fn sample_repl_instructions() -> Vec<String> {
    vec![
        r#"(workbook-sheets (path "grades.xlsx"))"#.to_string(),
        r#"(workbook-range (path "grades.xlsx") "Grades" 0 0 5 4)"#.to_string(),
        r#"(workbook-summary (path "grades.xlsx") "Grades" 20)"#.to_string(),
    ]
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
            println!(
                "These are leftover UUID session files, not the current resume model.\n\
                 Resume a named Brain with: finch attach <brain-name>\n\
                 Leftover files stay on disk until an explicit import."
            );
        }
    }
    Ok(())
}

fn reject_retired_session_flags(args: &Args) -> Result<()> {
    if args.resume.is_none() && args.restore_session.is_none() {
        return Ok(());
    }
    anyhow::bail!(
        "`--resume` and `--restore-session` are retired. Resume a named Brain with:\n  \
         finch attach <brain-name>\n\
         Leftover UUID session files remain in ~/.finch/sessions/ until an explicit import.\n\
         List them with: finch sessions list"
    )
}

fn resolve_brain_name(attach: Option<String>, flag: Option<String>) -> Result<String> {
    match attach.or(flag) {
        Some(name) => Ok(finch::brain::BrainStore::validate_name(&name)?.to_string()),
        None => Ok(finch::brain::generate()),
    }
}

/// Handle `finch brain` subcommands against the default on-disk store.
fn run_brain_command(cmd: BrainCommand) -> Result<()> {
    let store = finch::brain::BrainStore::new("cli");
    let daemon_running = if matches!(cmd, BrainCommand::Rm { .. }) {
        finch::daemon::DaemonLifecycle::new()?.is_running()
    } else {
        false
    };
    execute_brain_command(cmd, &store, daemon_running, &mut io::stdout())
}

fn execute_brain_command(
    cmd: BrainCommand,
    store: &finch::brain::BrainStore,
    daemon_running: bool,
    out: &mut impl Write,
) -> Result<()> {
    match cmd {
        BrainCommand::Ls { json } => list_named_brains(store, json, out),
        BrainCommand::Rm { name } => remove_named_brain(store, &name, daemon_running, out),
    }
}

fn list_named_brains(
    store: &finch::brain::BrainStore,
    json: bool,
    out: &mut impl Write,
) -> Result<()> {
    let summaries = store.list_summaries_unhydrated();
    if json {
        writeln!(out, "{}", serde_json::to_string(&summaries)?)?;
        return Ok(());
    }
    if summaries.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{:<32} {:>5} {:>8} {:>8} {:>6}",
        "NAME", "TURNS", "SIZE", "ATTACHED", "AGENTS"
    )?;
    for summary in summaries {
        writeln!(
            out,
            "{:<32} {:>5} {:>8} {:>8} {:>6}",
            summary.name,
            summary.turns,
            format_brain_bytes(summary.bytes),
            summary.attached.len(),
            summary.agents.len()
        )?;
    }
    Ok(())
}

fn format_brain_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}M", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1}K", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}B")
    }
}

fn remove_named_brain(
    store: &finch::brain::BrainStore,
    name: &str,
    daemon_running: bool,
    out: &mut impl Write,
) -> Result<()> {
    let name = finch::brain::BrainStore::validate_name(name)?.to_string();
    if !store.list_names_unhydrated().contains(&name) {
        anyhow::bail!("brain '{name}' not found");
    }
    if daemon_running {
        anyhow::bail!(
            "cannot remove brain '{name}' while the Finch daemon is running; \
             stop it first with: finch daemon-stop"
        );
    }
    match store.archive(&name)? {
        Some(archived_to) => {
            writeln!(out, "Archived '{name}' to {}", archived_to.display())?;
            Ok(())
        }
        None => anyhow::bail!("brain '{name}' not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        execute_brain_command, finish_first_run_setup, query_tool_state_paths,
        register_query_vm_tools, reject_retired_session_flags, resolve_brain_name,
        supervise_generator_loader_task, suppress_ort_logs_unless_overridden, Args, AuthCommand,
        BrainCommand, Command,
    };
    use clap::{CommandFactory, Parser};
    use finch::models::GeneratorState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn query_code_search_fails_closed_without_an_application_state_root() {
        assert!(query_tool_state_paths(None).is_err());
        assert_eq!(
            query_tool_state_paths(Some(std::path::PathBuf::from("/home/example"))).unwrap(),
            (
                std::path::PathBuf::from("/home/example/.finch/tool_patterns.json"),
                std::path::PathBuf::from("/home/example/.finch/source-index"),
            )
        );
    }

    /// #223: the daemon and interactive-mode call sites used to hardcode
    /// `ORT_LOGGING_LEVEL=3` unconditionally, so an operator setting it
    /// themselves for diagnostics had no effect. Both now call this shared
    /// helper, which must default to suppressed but never override a value
    /// the operator already set.
    #[test]
    fn suppress_ort_logs_defaults_but_does_not_override_operator_choice() {
        let before = std::env::var_os("ORT_LOGGING_LEVEL");

        std::env::remove_var("ORT_LOGGING_LEVEL");
        suppress_ort_logs_unless_overridden();
        assert_eq!(
            std::env::var("ORT_LOGGING_LEVEL").as_deref(),
            Ok("3"),
            "with no operator override, ORT_LOGGING_LEVEL must default to suppressed (Error/Fatal only)"
        );

        std::env::set_var("ORT_LOGGING_LEVEL", "0");
        suppress_ort_logs_unless_overridden();
        assert_eq!(
            std::env::var("ORT_LOGGING_LEVEL").as_deref(),
            Ok("0"),
            "an operator-set ORT_LOGGING_LEVEL must survive the daemon/interactive startup call, \
             not be silently stomped back to \"3\""
        );

        match before {
            Some(value) => std::env::set_var("ORT_LOGGING_LEVEL", value),
            None => std::env::remove_var("ORT_LOGGING_LEVEL"),
        }
    }

    /// The printed instructions must compile against the real runtime.
    ///
    /// `finch samples` advertised `xlsx@`, a word no user input could reach.
    /// The first replacement was wrong three ways -- `Sheet1`, which no sample
    /// workbook contains; two of `workbook-summary`'s three arguments; and an
    /// absolute path `Type::Path(./**)` rejects. The second was wrong a fourth
    /// way: a `/lisp` prefix that is not a command, so the line printed the
    /// help screen.
    ///
    /// The string-matching test that was supposed to prevent the second
    /// mistake could not see it -- it checked sheet names and a positional
    /// argument count, and never looked at the prefix. So this submits each
    /// line through `ProgramRuntime` instead of parsing it. A wrong word,
    /// arity, argument order, argument type or path form fails as `E-LINK-*`,
    /// `E-TYPE-*` or `E-PATH-*` long before the broker is reached; reaching
    /// `AuthorizationRequired` means everything a non-interactive test can
    /// check has passed, and only the user's approval remains.
    #[tokio::test]
    async fn run_samples_instructions_compile_and_reach_the_broker() {
        let dir = tempfile::tempdir().expect("tempdir");
        finch::samples::generate_all(dir.path()).expect("generate");

        for line in super::sample_repl_instructions() {
            assert!(
                !line.starts_with('/'),
                "a leading `/` is parsed as a REPL command, and an unrecognised \
                 one prints the help screen: {line}"
            );
            assert!(
                line.starts_with('('),
                "the REPL routes only a bare leading `(` to Lisp: {line}"
            );
            // And it must demonstrate a workbook word. Without this,
            // `(file-read (path "grades.xlsx"))` passes every other check --
            // it links, types, resolves the path and stops at approval -- while
            // showing nothing about reading a spreadsheet, which is what
            // `finch samples` exists to introduce.
            let word = line
                .trim_start_matches('(')
                .split_whitespace()
                .next()
                .unwrap_or("");
            assert!(
                word.starts_with("workbook-"),
                "a samples instruction must demonstrate a workbook word, not {word:?}: {line}"
            );

            // A sheet name is a runtime value, not a typed one: the broker only
            // discovers it is wrong after approval, so compiling the line
            // cannot catch `Sheet1`. Checked separately, against whichever
            // workbook the line actually names -- the sheets differ per file,
            // so reading them all from `grades.xlsx` would reject a correct
            // `budget.xlsx` line.
            let quoted: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
            let file = quoted
                .iter()
                .find(|value| value.ends_with(".xlsx"))
                .expect("an invocation names a workbook");
            let sheets = {
                use calamine::{open_workbook_auto, Reader};
                open_workbook_auto(dir.path().join(file))
                    .expect("sample workbook opens")
                    .sheet_names()
                    .to_vec()
            };
            for sheet in quoted.iter().filter(|value| !value.ends_with(".xlsx")) {
                assert!(
                    sheets.contains(&sheet.to_string()),
                    "{line}\n  names sheet {sheet:?}, but {file} has {sheets:?}"
                );
            }

            let runtime = finch::runtime::ProgramRuntime::new();
            let outcome = runtime
                .submit_typed_only(finch::runtime::ProgramSubmission {
                    language: finch_programs::ProgramLanguage::Lisp,
                    source_id: Some("samples-instruction.lisp".to_string()),
                    source: line.clone(),
                    intent: "check a printed instruction".to_string(),
                    effect: finch_programs::ExecutionEffect::Unclassified,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                })
                .await
                .expect("submit");

            let diagnostics = outcome.diagnostics.join("; ");
            for code in ["E-LINK", "E-TYPE", "E-PATH", "E-FORTH"] {
                assert!(
                    !diagnostics.contains(code),
                    "{line}\n  failed with {code}: {diagnostics}"
                );
            }
            assert_eq!(
                outcome.status,
                finch::runtime::ExecutionStatus::AuthorizationRequired,
                "{line}\n  diagnostics: {diagnostics}"
            );
        }
    }

    #[test]
    fn legacy_coforth_and_exchange_subcommands_are_not_public() {
        assert!(Args::try_parse_from(["finch", "coforth", "run", "--code", "1 2 +"]).is_err());
        assert!(Args::try_parse_from(["finch", "exchange", "list"]).is_err());
        assert!(Args::try_parse_from(["finch", "--peer", "peer.example:8000"]).is_err());
        assert!(Args::try_parse_from(["finch", "library", "verify"]).is_err());
        assert!(Args::try_parse_from(["finch", "library", "heal"]).is_err());
        assert!(Args::try_parse_from(["finch", "library", "build", "--all"]).is_err());
    }

    #[tokio::test]
    async fn cancelled_first_run_reports_read_only_outcome_without_success_claim() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(&config_path, "ordered-provider-graph-sentinel").unwrap();
        let validate_calls = Arc::new(AtomicUsize::new(0));
        let load_calls = Arc::new(AtomicUsize::new(0));
        let validate_probe = validate_calls.clone();
        let load_probe = load_calls.clone();
        let destructive_path = config_path.clone();
        let mut success_output = Vec::new();

        let error = finish_first_run_setup(
            Err(anyhow::anyhow!("Setup cancelled")),
            move |_| async move {
                validate_probe.fetch_add(1, Ordering::SeqCst);
                Ok(finch::cli::SetupApplyOutcome::Saved)
            },
            move || {
                load_probe.fetch_add(1, Ordering::SeqCst);
                std::fs::write(&destructive_path, "overwritten")?;
                anyhow::bail!("load callback must not run after cancellation")
            },
            &mut success_output,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Setup cancelled; no configuration was saved"
        );
        assert_eq!(validate_calls.load(Ordering::SeqCst), 0);
        assert_eq!(load_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read_to_string(config_path).unwrap(),
            "ordered-provider-graph-sentinel"
        );
        assert!(success_output.is_empty());
    }

    #[test]
    fn chatgpt_auth_cli_parses_exact_status_login_logout_and_rejects_unsafe_references() {
        let status = Args::try_parse_from([
            "finch",
            "auth",
            "status",
            "chatgpt",
            "--credential",
            "chatgpt:work",
        ])
        .unwrap();
        assert!(matches!(
            status.command,
            Some(Command::Auth {
                auth_command: AuthCommand::Status { credential, .. }
            }) if credential == "chatgpt:work"
        ));

        let login = Args::try_parse_from([
            "finch",
            "auth",
            "login",
            "--credential",
            "chatgpt:personal",
            "--copy",
            "--open",
        ])
        .unwrap();
        assert!(matches!(
            login.command,
            Some(Command::Auth {
                auth_command: AuthCommand::Login {
                    credential,
                    copy: true,
                    open: true,
                    ..
                }
            }) if credential == "chatgpt:personal"
        ));

        let logout = Args::try_parse_from(["finch", "auth", "logout"]).unwrap();
        assert!(matches!(
            logout.command,
            Some(Command::Auth {
                auth_command: AuthCommand::Logout { credential, .. }
            }) if credential == "chatgpt:default"
        ));

        let recover = Args::try_parse_from([
            "finch",
            "auth",
            "recover",
            "chatgpt",
            "--credential",
            "chatgpt:work",
        ])
        .unwrap();
        assert!(matches!(
            recover.command,
            Some(Command::Auth {
                auth_command: AuthCommand::Recover { credential, .. }
            }) if credential == "chatgpt:work"
        ));

        for hostile in ["../codex", "chatgpt/work", "chatgpt work", ""] {
            assert!(
                Args::try_parse_from(["finch", "auth", "status", "--credential", hostile,])
                    .is_err()
            );
        }
        assert!(Args::try_parse_from(["finch", "auth", "login", "openai"]).is_err());
        assert!(Args::try_parse_from(["finch", "auth", "login", "grok"]).is_err());

        let grok = Args::try_parse_from(["finch", "auth", "login", "grok-sub"]).unwrap();
        assert!(matches!(
            grok.command,
            Some(Command::Auth {
                auth_command: AuthCommand::Login {
                    provider,
                    credential,
                    ..
                }
            }) if provider == "grok-sub" && credential == "grok-sub:default"
        ));
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

    #[test]
    fn brain_ls_json_and_rm_parse_as_named_brain_commands() {
        let ls = Args::try_parse_from(["finch", "brain", "ls", "--json"]).unwrap();
        assert!(
            matches!(
                ls.command,
                Some(Command::Brain {
                    brain_command: BrainCommand::Ls { json: true }
                })
            ),
            "finch brain ls --json must set the subcommand JSON flag, not a global one: {ls:?}"
        );

        let list = Args::try_parse_from(["finch", "brain", "list"]).unwrap();
        assert!(
            matches!(
                list.command,
                Some(Command::Brain {
                    brain_command: BrainCommand::Ls { json: false }
                })
            ),
            "list is an alias for ls: {list:?}"
        );

        let rm = Args::try_parse_from(["finch", "brain", "rm", "golden-ridge-0771a6"]).unwrap();
        assert!(
            matches!(
                rm.command,
                Some(Command::Brain {
                    brain_command: BrainCommand::Rm { ref name }
                }) if name == "golden-ridge-0771a6"
            ),
            "finch brain rm <name> must capture the Brain name: {rm:?}"
        );

        let remove = Args::try_parse_from(["finch", "brain", "remove", "old-project"]).unwrap();
        assert!(
            matches!(
                remove.command,
                Some(Command::Brain {
                    brain_command: BrainCommand::Rm { ref name }
                }) if name == "old-project"
            ),
            "remove is an alias for rm: {remove:?}"
        );
    }

    fn isolated_brain_store() -> (tempfile::TempDir, finch::brain::BrainStore) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("brains");
        std::fs::create_dir_all(&root).expect("create brains root");
        let store = finch::brain::BrainStore::with_root("cli-test", Some(root));
        (temp, store)
    }

    fn plant_unhydrated_brain(store: &finch::brain::BrainStore, name: &str) {
        let root = store.root().expect("on-disk store");
        std::fs::create_dir_all(root.join(name)).expect("plant Brain directory");
    }

    #[test]
    fn brain_ls_emits_sorted_names_without_hydrating_or_creating_files() {
        let (_temp, store) = isolated_brain_store();
        plant_unhydrated_brain(&store, "zeta");
        plant_unhydrated_brain(&store, "alpha");
        std::fs::write(store.root().unwrap().join("not-a-brain.json"), "{}")
            .expect("plant ignored file");

        let mut out = Vec::new();
        execute_brain_command(BrainCommand::Ls { json: false }, &store, false, &mut out)
            .expect("list named Brains");
        let text = String::from_utf8(out).expect("utf8");
        let names: Vec<&str> = text
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert_eq!(
            names,
            ["alpha", "zeta"],
            "text ls must emit a header then sorted names, ignoring non-directories; got {text:?}"
        );
        assert!(
            text.contains("TURNS")
                && text.contains("SIZE")
                && text.contains("ATTACHED")
                && text.contains("AGENTS"),
            "text ls must show turns, size, attachments and agents; got {text:?}"
        );

        let metadata = store.root().unwrap().join("alpha").join("metadata.json");
        assert!(
            !metadata.exists(),
            "ls must not hydrate; {} would mean ensure_loaded ran",
            metadata.display()
        );
    }

    #[test]
    fn brain_ls_json_includes_name_and_on_disk_path() {
        let (_temp, store) = isolated_brain_store();
        plant_unhydrated_brain(&store, "golden-ridge-0771a6");

        let mut out = Vec::new();
        execute_brain_command(BrainCommand::Ls { json: true }, &store, false, &mut out)
            .expect("list named Brains as JSON");
        let text = String::from_utf8(out).expect("utf8");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(text.trim()).expect("ls --json must be a JSON array");
        assert_eq!(parsed.len(), 1, "expected one Brain in {text}");
        assert_eq!(
            parsed[0]["name"].as_str(),
            Some("golden-ridge-0771a6"),
            "JSON name field: {text}"
        );
        let expected_path = store
            .root()
            .unwrap()
            .join("golden-ridge-0771a6")
            .display()
            .to_string();
        assert_eq!(
            parsed[0]["path"].as_str(),
            Some(expected_path.as_str()),
            "JSON path must be the live Brain directory; got {text}"
        );
        assert_eq!(
            parsed[0]["turns"].as_u64(),
            Some(0),
            "an empty Brain directory has no Prompt events; got {text}"
        );
        assert_eq!(
            parsed[0]["bytes"].as_u64(),
            Some(0),
            "an empty Brain directory has no files; got {text}"
        );
        assert_eq!(
            parsed[0]["attached"].as_array().map(|items| items.len()),
            Some(0),
            "an empty Brain directory has no attachments; got {text}"
        );
        assert_eq!(
            parsed[0]["agents"].as_array().map(|items| items.len()),
            Some(0),
            "an empty Brain directory has no live subagents; got {text}"
        );
    }

    #[test]
    fn brain_ls_json_reports_turns_size_attachments_and_live_agents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("brains");
        let writer = finch::brain::BrainStore::with_root("cli-test", Some(root.clone()));
        writer
            .push(
                "busy-brain",
                "alice",
                finch::brain::BrainEventKind::Prompt {
                    text: "hello".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .expect("prompt");
        writer
            .push(
                "busy-brain",
                "alice",
                finch::brain::BrainEventKind::Prompt {
                    text: "again".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .expect("second prompt");
        let snapshot = writer.snapshot("busy-brain").expect("snapshot");
        let prompt_seq = snapshot.events.last().expect("prompt event").seq;
        let attachment = writer
            .attach(
                "busy-brain",
                "alice@box.local",
                finch::brain::AttachmentRole::Driver,
                None,
            )
            .expect("attach");
        writer
            .activate_connection(
                "busy-brain",
                attachment.attachment_id,
                attachment.connection_id.expect("connection"),
            )
            .expect("activate");
        writer
            .start_run(
                "busy-brain",
                "alice@box.local",
                finch::brain::BrainRunKind::Subagent,
                prompt_seq,
                attachment.attachment_id,
                finch::brain::BrainRunStatus::Running,
            )
            .expect("start subagent");

        let store = finch::brain::BrainStore::with_root("cli-test", Some(root));

        let mut out = Vec::new();
        execute_brain_command(BrainCommand::Ls { json: true }, &store, false, &mut out)
            .expect("list named Brains as JSON");
        let text = String::from_utf8(out).expect("utf8");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(text.trim()).expect("ls --json must be a JSON array");
        assert_eq!(parsed.len(), 1, "expected one Brain in {text}");
        let entry = &parsed[0];
        assert_eq!(
            entry["name"].as_str(),
            Some("busy-brain"),
            "JSON name: {text}"
        );
        assert_eq!(
            entry["turns"].as_u64(),
            Some(2),
            "each Prompt event is one turn; got {text}"
        );
        let bytes = entry["bytes"].as_u64().expect("bytes");
        assert!(
            bytes > 0,
            "a Brain with an event log must report a positive size; got {text}"
        );
        let attached = entry["attached"].as_array().expect("attached array");
        assert_eq!(
            attached.len(),
            1,
            "activated driver must appear as attached; got {text}"
        );
        assert_eq!(
            attached[0]["subject"].as_str(),
            Some("alice@box.local"),
            "attached subject: {text}"
        );
        assert_eq!(
            attached[0]["role"].as_str(),
            Some("driver"),
            "attached role: {text}"
        );
        let agents = entry["agents"].as_array().expect("agents array");
        assert_eq!(
            agents.len(),
            1,
            "a running Subagent run is a live agent; got {text}"
        );
        assert_eq!(
            agents[0]["status"].as_str(),
            Some("running"),
            "live agent status: {text}"
        );
        assert_eq!(
            agents[0]["initiated_by"].as_str(),
            Some("alice@box.local"),
            "live agent initiator: {text}"
        );
    }

    #[test]
    fn brain_ls_json_empty_store_emits_empty_array() {
        let (_temp, store) = isolated_brain_store();
        let mut out = Vec::new();
        execute_brain_command(BrainCommand::Ls { json: true }, &store, false, &mut out)
            .expect("list empty store");
        assert_eq!(
            String::from_utf8(out).expect("utf8").trim(),
            "[]",
            "empty JSON ls must be an empty array for cleanup scripts"
        );
    }

    #[test]
    fn brain_rm_archives_named_brain_when_daemon_is_stopped() {
        let (_temp, store) = isolated_brain_store();
        plant_unhydrated_brain(&store, "scratch-brain");
        std::fs::write(
            store
                .root()
                .unwrap()
                .join("scratch-brain")
                .join("events.jsonl"),
            "{\"seq\":1}\n",
        )
        .expect("plant a log so archive has something to keep");

        let mut out = Vec::new();
        execute_brain_command(
            BrainCommand::Rm {
                name: "scratch-brain".into(),
            },
            &store,
            false,
            &mut out,
        )
        .expect("archive named Brain");
        let text = String::from_utf8(out).expect("utf8");
        assert!(
            text.contains("Archived 'scratch-brain' to") && text.contains("brains-archive"),
            "rm must report the archive destination for '{text}'"
        );
        assert!(
            !store.root().unwrap().join("scratch-brain").exists(),
            "live directory must be gone after rm; listing is {:?}, path {}",
            store.list_names_unhydrated(),
            store.root().unwrap().join("scratch-brain").display()
        );
        assert_eq!(
            store.list_names_unhydrated(),
            Vec::<String>::new(),
            "ls membership after rm must be empty, got {:?}",
            store.list_names_unhydrated()
        );
        let archive_root = store
            .root()
            .unwrap()
            .parent()
            .unwrap()
            .join("brains-archive");
        let archived: Vec<_> = std::fs::read_dir(&archive_root)
            .unwrap_or_else(|error| panic!("read {}: {error}", archive_root.display()))
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(
            archived.len(),
            1,
            "exactly one archive directory under {}",
            archive_root.display()
        );
        let archived_name = archived[0].file_name();
        let archived_name = archived_name.to_string_lossy();
        assert!(
            archived_name.starts_with("scratch-brain-"),
            "archive directory must keep the Brain name, got {archived_name}"
        );
        assert!(
            archived[0].path().join("events.jsonl").exists(),
            "archive must preserve the event log at {}",
            archived[0].path().display()
        );
    }

    #[test]
    fn brain_rm_refuses_while_daemon_is_running_and_leaves_the_brain() {
        let (_temp, store) = isolated_brain_store();
        plant_unhydrated_brain(&store, "live-brain");

        let mut out = Vec::new();
        let error = execute_brain_command(
            BrainCommand::Rm {
                name: "live-brain".into(),
            },
            &store,
            true,
            &mut out,
        )
        .expect_err("rm must fail closed while the daemon is running");
        assert_eq!(
            error.to_string(),
            "cannot remove brain 'live-brain' while the Finch daemon is running; \
             stop it first with: finch daemon-stop",
            "daemon-running refusal must name the Brain and the stop command"
        );
        assert!(
            out.is_empty(),
            "failed rm must not print a success line, got {}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            store.root().unwrap().join("live-brain").is_dir(),
            "live Brain directory must remain after a refused rm"
        );
        assert_eq!(
            store.list_names_unhydrated(),
            vec!["live-brain".to_string()],
            "ls membership must be unchanged after a refused rm, got {:?}",
            store.list_names_unhydrated()
        );
    }

    #[test]
    fn brain_rm_missing_name_fails_without_touching_the_store() {
        let (_temp, store) = isolated_brain_store();
        plant_unhydrated_brain(&store, "kept");

        let mut out = Vec::new();
        let error = execute_brain_command(
            BrainCommand::Rm {
                name: "missing-brain".into(),
            },
            &store,
            false,
            &mut out,
        )
        .expect_err("rm of an unknown Brain must fail");
        assert_eq!(
            error.to_string(),
            "brain 'missing-brain' not found",
            "missing-name error must identify the requested Brain"
        );
        assert_eq!(
            store.list_names_unhydrated(),
            vec!["kept".to_string()],
            "an unknown rm must not archive a different Brain, got {:?}",
            store.list_names_unhydrated()
        );
    }

    #[test]
    fn brain_rm_invalid_name_fails_before_archive() {
        let (_temp, store) = isolated_brain_store();
        let mut out = Vec::new();
        let error = execute_brain_command(
            BrainCommand::Rm {
                name: "../other".into(),
            },
            &store,
            false,
            &mut out,
        )
        .expect_err("path-like names must be rejected");
        assert_eq!(
            error.to_string(),
            "brain name must use 1-64 letters, numbers, '-' or '_'",
            "invalid-name error must use the store's validate_name message"
        );
    }

    #[test]
    fn attach_subcommand_parses_a_brain_name() {
        let args = Args::try_parse_from(["finch", "attach", "golden-ridge-0771a6"]).unwrap();
        match args.command {
            Some(Command::Attach {
                name,
                model,
                provider,
            }) => {
                assert_eq!(
                    name, "golden-ridge-0771a6",
                    "finch attach NAME must parse the Brain name"
                );
                assert_eq!(model, None, "bare attach must not invent a --model overlay");
                assert_eq!(
                    provider, None,
                    "bare attach must not invent a --provider binding"
                );
            }
            other => panic!("finch attach NAME must parse as the attach subcommand, got {other:?}"),
        }
    }

    #[test]
    fn brain_flag_remains_a_hidden_compatibility_alias() {
        let args = Args::try_parse_from(["finch", "--brain", "golden-ridge-0771a6"]).unwrap();
        assert_eq!(
            args.brain.as_deref(),
            Some("golden-ridge-0771a6"),
            "--brain NAME must still parse as the compatibility alias"
        );
        assert!(
            args.command.is_none(),
            "--brain must not be a subcommand, got {:?}",
            args.command
        );
    }

    #[test]
    fn attach_and_brain_flag_resolve_to_the_same_validated_name() {
        assert_eq!(
            resolve_brain_name(Some("golden-ridge-0771a6".into()), None).unwrap(),
            "golden-ridge-0771a6"
        );
        assert_eq!(
            resolve_brain_name(None, Some("golden-ridge-0771a6".into())).unwrap(),
            "golden-ridge-0771a6"
        );
        assert_eq!(
            resolve_brain_name(Some("canonical".into()), Some("alias".into())).unwrap(),
            "canonical",
            "the attach subcommand is canonical when both are supplied"
        );
    }

    #[test]
    fn hostile_brain_names_are_rejected_before_repl() {
        for name in ["foo;rm", "foo`id`", "foo\nbar", "\u{1b}[31mred", "foo bar"] {
            let error = resolve_brain_name(Some(name.into()), None)
                .expect_err("hostile names must not become resume arguments");
            assert_eq!(
                error.to_string(),
                "brain name must use 1-64 letters, numbers, '-' or '_'",
                "hostile name {name:?} must fail closed with validate_name's message"
            );
        }
    }

    #[test]
    fn help_advertises_attach_not_uuid_resume() {
        let help = Args::command().render_long_help().to_string();
        assert!(
            help.contains("attach"),
            "top-level help must list `attach`, got:\n{help}"
        );
        assert!(
            !help.contains("--resume"),
            "top-level help must not advertise retired --resume, got:\n{help}"
        );
        assert!(
            !help.contains("--restore-session"),
            "top-level help must not advertise retired --restore-session, got:\n{help}"
        );
        assert!(
            !help.contains("--brain"),
            "top-level help must not advertise the hidden --brain alias, got:\n{help}"
        );
    }

    #[test]
    fn retired_resume_flags_fail_with_attach_guidance() {
        let mut args =
            Args::try_parse_from(["finch", "--resume", "2fdae496-60c2-41b1-a901-857af8f0ed82"])
                .unwrap();
        let error = reject_retired_session_flags(&args)
            .expect_err("retired --resume must not enter the REPL");
        let message = error.to_string();
        assert!(
            message.contains("finch attach"),
            "the retirement error must name the replacement command, got {message}"
        );
        assert!(
            !message.contains("sessions/<uuid>"),
            "the retirement error must not teach UUID resume, got {message}"
        );

        args = Args::try_parse_from(["finch", "--restore-session", "/tmp/old.json"]).unwrap();
        let error = reject_retired_session_flags(&args)
            .expect_err("retired --restore-session must not enter the REPL");
        assert!(
            error.to_string().contains("finch attach"),
            "the retirement error must name the replacement command, got {}",
            error
        );
    }

    /// #905: the model-loader task in `run_daemon` used to be spawned
    /// fire-and-forget (`tokio::spawn(async move { .. });` with the
    /// `JoinHandle` discarded). `BootstrapLoader::load_generator_async` only
    /// catches a panic inside its own `spawn_blocking` call; a panic
    /// anywhere else in the outer async block -- reproduced here with a
    /// task that panics directly, standing in for the real loader per the
    /// no-live-model-load constraint -- used to be silently swallowed by
    /// Tokio's default panic hook, leaving `generator_state` stuck at
    /// whatever it last was (here, `Initializing`) forever, with the
    /// "wait for Ready" poller in `run_daemon` spinning with no diagnostic
    /// signal. `supervise_generator_loader_task` closes that gap by
    /// awaiting the `JoinHandle` and setting `GeneratorState::Failed` on any
    /// `JoinError`. This test awaits the supervisor's own completion
    /// directly (no sleep) and asserts the resulting state, not a timing
    /// ratio.
    #[tokio::test]
    async fn supervisor_marks_generator_failed_when_loader_task_panics() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let panicking_task = tokio::spawn(async move {
            panic!("simulated panic outside the guarded spawn_blocking call");
        });

        // Awaiting the supervisor's own completion is the deterministic
        // signal here: it does not return until it has observed the
        // JoinHandle resolve and has written the resulting state.
        supervise_generator_loader_task(panicking_task, Arc::clone(&state)).await;

        let observed = state.read().await;
        match &*observed {
            GeneratorState::Failed { error } => {
                assert!(
                    error.contains("panicked"),
                    "failure message must name that the loader task itself panicked \
                     (distinct from the already-handled spawn_blocking panic path), got: {error}"
                );
            }
            other => panic!(
                "expected GeneratorState::Failed after the supervised task panicked, \
                 got {other:?} -- a panic outside the guarded spawn_blocking call must \
                 not leave the state stuck forever"
            ),
        }
    }

    /// Companion to the panic case above: when the supervised task
    /// completes normally, the supervisor must not touch the state at all
    /// (the loader itself, or its `Err` branch in `run_daemon`, owns every
    /// non-panic transition).
    #[tokio::test]
    async fn supervisor_leaves_state_untouched_when_loader_task_completes_normally() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let normal_task = tokio::spawn(async move {});

        supervise_generator_loader_task(normal_task, Arc::clone(&state)).await;

        let observed = state.read().await;
        assert!(
            matches!(&*observed, GeneratorState::Initializing),
            "a normally-completing loader task must not have its state touched by the \
             supervisor, got {observed:?}"
        );
    }
}
