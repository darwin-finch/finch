// Interactive REPL with Claude Code-style interface

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    style::Stylize,
    terminal::{self, Clear, ClearType},
    ExecutableCommand,
};
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};

use crate::claude::{ClaudeClient, MessageRequest};
use crate::config::Config;
use crate::local::LocalGenerator;
use crate::metrics::{MetricsLogger, RequestMetric, ResponseComparison, TrainingTrends};
use crate::models::tokenizer::TextTokenizer;
use crate::models::ThresholdValidator;
use crate::models::{
    BootstrapLoader, GeneratorState, Sampler, SamplingConfig, TrainingCoordinator, WeightedExample,
};
use crate::providers::{TeacherContextConfig, TeacherSession};
use crate::router::{ForwardReason, RouteDecision, Router};
use crate::tools::executor::{generate_tool_signature, ApprovalSource, ToolSignature};
use crate::tools::implementations::{
    AskUserQuestionTool, BashTool, EditTool, EnterPlanModeTool, GlobTool, GrepTool,
    PatchTool, PresentPlanTool, ReadTool, RestartTool, SaveAndExecTool, WebFetchTool, WriteTool,
};
#[cfg(target_os = "macos")]
use crate::tools::implementations::{GuiClickTool, GuiInspectTool, GuiTypeTool};
use crate::tools::patterns::ToolPattern;
use crate::tools::types::{ToolDefinition, ToolUse};
use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};
use crate::training::batch_trainer::BatchTrainer;

use super::commands::{handle_command, Command, CommandOutput};
use super::conversation::ConversationHistory;
use super::input::InputHandler;
use super::menu::{Menu, MenuOption};
use super::output_manager::OutputManager;
use super::status_bar::StatusBar;
use super::tui::TuiRenderer;

// Phase 3.5: Import output macros for global output routing
use crate::{output_error, output_status};

/// User's menu choice for tool confirmation
#[derive(Debug, Clone)]
enum ConfirmationChoice {
    ApproveOnce,
    ApproveExactSession,
    ApprovePatternSession,
    ApproveExactPersistent,
    ApprovePatternPersistent,
    Deny,
}

// Use ConfirmationResult from event loop module
use super::repl_event::ConfirmationResult;

/// Get current terminal width, or default to 80 if not a TTY
fn terminal_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// REPL operating mode
#[derive(Debug, Clone, PartialEq)]
pub enum ReplMode {
    /// Normal mode - all tools require confirmation
    Normal,
    /// Planning mode - only inspection tools allowed (read, glob, grep, web_fetch)
    Planning {
        task: String,
        plan_path: PathBuf,
        created_at: chrono::DateTime<chrono::Utc>,
    },
    /// Executing mode - all tools enabled after plan approval
    Executing {
        task: String,
        plan_path: PathBuf,
        approved_at: chrono::DateTime<chrono::Utc>,
    },
}

#[allow(dead_code)]
pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    // Daemon client (optional - for daemon-only mode)
    daemon_client: Option<Arc<crate::client::DaemonClient>>,
    // Teacher session with context optimization
    teacher_session: Arc<RwLock<TeacherSession>>,
    // Provider management (for runtime switching)
    available_providers: Vec<crate::config::ProviderEntry>,
    available_teachers: Vec<crate::config::TeacherEntry>,
    active_provider_index: usize,
    active_teacher_index: usize,
    router: Router, // Now contains ThresholdRouter
    metrics_logger: MetricsLogger,
    // Online learning models
    threshold_validator: ThresholdValidator, // Keep validator separate
    local_generator: Arc<RwLock<crate::local::LocalGenerator>>, // Local generation
    // Training metrics
    training_trends: TrainingTrends,
    // Model persistence
    models_dir: Option<PathBuf>,
    // Qwen model bootstrap (progressive loading)
    bootstrap_loader: Arc<BootstrapLoader>,
    tokenizer: Arc<crate::models::tokenizer::TextTokenizer>,
    // Tool execution
    tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
    tool_definitions: Vec<ToolDefinition>, // Cached tool definitions for Claude API
    // UI state
    is_interactive: bool,
    streaming_enabled: bool,
    debug_enabled: bool,
    // Readline input handler
    input_handler: Option<InputHandler>,
    // Conversation history
    conversation: Arc<RwLock<ConversationHistory>>,
    // REPL mode (normal, planning, executing)
    mode: ReplMode,
    // LoRA fine-tuning (NEW)
    training_coordinator: Arc<TrainingCoordinator>,
    sampler: Arc<RwLock<Sampler>>,
    // Track last exchange for feedback
    last_query: Option<String>,
    last_response: Option<String>,
    last_was_sampled: bool,
    // Output management (Phase 1: Terminal UI refactor)
    output_manager: OutputManager,
    status_bar: StatusBar,
    // TUI renderer moved to global (Phase 5: Native ratatui dialogs)

    // Phase 1: Multi-LLM system
    llm_registry: Option<Arc<crate::llms::LLMRegistry>>,
    conversation_logger: Arc<tokio::sync::Mutex<crate::logging::ConversationLogger>>,

    // Phase 2: Persona system
    active_persona: Arc<RwLock<crate::config::Persona>>,

    // Phase 4: Hierarchical memory system
    memory_system: Option<Arc<crate::memory::MemorySystem>>,
}

/// Background training statistics
struct BackgroundTrainingStats {
    examples_trained: usize,
    queue_path: String,
}

#[allow(dead_code)]
impl Repl {
    pub async fn new(
        config: Config,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
    ) -> Self {
        // Detect if we're in interactive mode (stdout is a TTY)
        let is_interactive = io::stdout().is_terminal();

        // Determine if we're in daemon mode (suppress local model logs)
        let daemon_mode = daemon_client.is_some();

        // Set up models directory
        let models_dir = dirs::home_dir().map(|home| home.join(".finch").join("models"));

        // Load validator only (router is now in Router)
        let threshold_validator = Self::load_validator(models_dir.as_ref(), is_interactive, daemon_mode);

        // Initialize input handler for interactive mode
        let input_handler = if is_interactive {
            match InputHandler::new() {
                Ok(handler) => Some(handler),
                Err(e) => {
                    output_status!("⚠️  Failed to initialize readline: {}", e);
                    output_status!("   Falling back to basic input mode");
                    None
                }
            }
        } else {
            None
        };

        // Phase 4: Initialize memory system (before tool registry so we can register memory tools)
        let memory_system = if config.memory.enabled {
            match crate::memory::MemorySystem::new(config.memory.clone()) {
                Ok(system) => {
                    if is_interactive && !daemon_mode {
                        output_status!("✓ Memory system enabled");
                    }
                    Some(Arc::new(system))
                }
                Err(e) => {
                    output_status!("⚠️  Failed to initialize memory: {}", e);
                    output_status!("   Continuing without memory");
                    None
                }
            }
        } else {
            None
        };

        // Initialize tool execution system
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(ReadTool));
        tool_registry.register(Box::new(GlobTool));
        tool_registry.register(Box::new(GrepTool));
        tool_registry.register(Box::new(WebFetchTool::new()));
        tool_registry.register(Box::new(BashTool));
        tool_registry.register(Box::new(EditTool));
        tool_registry.register(Box::new(PatchTool));
        tool_registry.register(Box::new(WriteTool));

        // Self-improvement tools
        let session_state_file = dirs::home_dir()
            .map(|home| home.join(".finch").join("restart_state.json"))
            .unwrap_or_else(|| PathBuf::from(".finch/restart_state.json"));

        tool_registry.register(Box::new(RestartTool::new(session_state_file.clone())));
        tool_registry.register(Box::new(SaveAndExecTool::new(session_state_file.clone())));

        // Plan mode tools
        tool_registry.register(Box::new(EnterPlanModeTool));
        tool_registry.register(Box::new(PresentPlanTool));

        // User interaction tools
        tool_registry.register(Box::new(AskUserQuestionTool));

        // Phase 1: Initialize LLM registry (before ToolExecutor creation)
        let llm_registry = if config.teachers.len() > 1 {
            match crate::llms::LLMRegistry::from_teachers(&config.teachers) {
                Ok(registry) => {
                    if is_interactive && !daemon_mode {
                        output_status!(
                            "✓ Multi-LLM system enabled ({} primary + {} tools)",
                            1,
                            config.teachers.len() - 1
                        );
                    }
                    Some(Arc::new(registry))
                }
                Err(e) => {
                    output_status!("⚠️  Failed to create LLM registry: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Phase 1: Register LLM delegation tools if multi-LLM is configured
        if let Some(ref registry) = llm_registry {
            let llm_tools = crate::tools::implementations::llm_tools::create_llm_tools(registry);
            for tool in llm_tools {
                tool_registry.register(tool);
            }
            if is_interactive && !daemon_mode {
                output_status!("✓ LLM delegation tools registered ({} tools)", registry.tool_names().len());
            }
        }

        // GUI automation tools (macOS only)
        #[cfg(target_os = "macos")]
        {
            if config.features.gui_automation {
                tracing::info!("Registering GUI automation tools (macOS)");
                tool_registry.register(Box::new(GuiClickTool));
                tool_registry.register(Box::new(GuiTypeTool));
                tool_registry.register(Box::new(GuiInspectTool));
            }
        }

        // Phase 4: Register memory tools if memory system is enabled
        if let Some(ref memory) = memory_system {
            use crate::tools::implementations::{SearchMemoryTool, CreateMemoryTool, ListRecentTool};
            tool_registry.register(Box::new(SearchMemoryTool::new(memory.clone())));
            tool_registry.register(Box::new(CreateMemoryTool::new(memory.clone())));
            tool_registry.register(Box::new(ListRecentTool::new(memory.clone())));
            if is_interactive && !daemon_mode {
                output_status!("✓ Memory tools registered (search_memory, create_memory, list_recent_memories)");
            }
        }

        // Create permission manager
        // Use config.features.auto_approve_tools to determine default rule
        let default_rule = if config.features.auto_approve_tools {
            PermissionRule::Allow // Auto-approve: skip confirmations
        } else {
            PermissionRule::Ask // Default: require user confirmation
        };
        let permissions = PermissionManager::new().with_default_rule(default_rule);

        // Determine patterns path
        let patterns_path = dirs::home_dir()
            .map(|home| home.join(".finch").join("tool_patterns.json"))
            .unwrap_or_else(|| PathBuf::from(".finch/tool_patterns.json"));

        // Create tool executor
        let executor = ToolExecutor::new(tool_registry, permissions, patterns_path)
            .unwrap_or_else(|e| {
                output_status!("⚠️  Failed to initialize tool executor: {}", e);
                output_status!("   Tool pattern persistence may not work correctly");
                // Create fresh registry and try with temp path
                let mut fallback_registry = ToolRegistry::new();
                fallback_registry.register(Box::new(ReadTool));
                fallback_registry.register(Box::new(GlobTool));
                fallback_registry.register(Box::new(GrepTool));
                fallback_registry.register(Box::new(WebFetchTool::new()));
                fallback_registry.register(Box::new(BashTool));
                fallback_registry.register(Box::new(RestartTool::new(session_state_file.clone())));
                fallback_registry
                    .register(Box::new(SaveAndExecTool::new(session_state_file.clone())));
                // Plan mode tools
                fallback_registry.register(Box::new(EnterPlanModeTool));
                fallback_registry.register(Box::new(PresentPlanTool));
                // User interaction tools
                fallback_registry.register(Box::new(AskUserQuestionTool));
                ToolExecutor::new(
                    fallback_registry,
                    PermissionManager::new().with_default_rule(PermissionRule::Allow),
                    std::env::temp_dir().join("finch_patterns_fallback.json"),
                )
                .expect("Failed to create fallback tool executor")
            });

        // Add MCP support if configured (graceful - always returns even on error)
        let executor = executor.with_mcp(&config).await;

        let tool_executor = Arc::new(tokio::sync::Mutex::new(executor));

        // Only show tool execution log in standalone mode (not daemon mode)
        if is_interactive && !daemon_mode {
            output_status!(
                "✓ Tool execution enabled ({} built-in tools)",
                tool_executor.lock().await.registry().len()
            );
        }

        let streaming_enabled = config.features.streaming_enabled;

        // Generate tool definitions from registry (includes built-in + MCP tools)
        let tool_definitions: Vec<ToolDefinition> = tool_executor
            .lock()
            .await
            .list_all_tools()
            .await;

        // Get global OutputManager and StatusBar (created in main.rs)
        // DO NOT create new instances - that would break stdout control!
        use crate::cli::global_output::{global_output, global_status};
        let output_manager_arc = global_output();
        let output_manager = (*output_manager_arc).clone(); // Clone for local reference
        let status_bar_arc = global_status();
        let status_bar = (*status_bar_arc).clone();

        // CRITICAL: If TUI will be enabled, disable stdout NOW (before ANY output)
        // This must happen before tokenizer init, bootstrap spawn, or any other code
        // that might emit tracing logs.
        if config.tui_enabled && is_interactive {
            // Disable stdout on the global OutputManager
            // All output will go to buffer; TUI will render via insert_before()
            (*output_manager_arc).disable_stdout();
        }

        // Initialize tokenizer (still needed for tool execution)
        let tokenizer = Arc::new(TextTokenizer::default().unwrap_or_else(|e| {
            output_status!("⚠️  Failed to create tokenizer: {}", e);
            output_status!("   Active learning tools may not work correctly");
            panic!("Cannot create tokenizer")
        }));

        // NOTE: Model loading removed - daemon handles all local inference
        // Create placeholder bootstrap_loader and local_generator for compatibility
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        let bootstrap_loader = Arc::new(BootstrapLoader::new(
            Arc::clone(&generator_state),
            Some(Arc::new(output_manager.clone())),
        ));
        let local_generator = Arc::new(RwLock::new(LocalGenerator::new()));

        // Initialize LoRA fine-tuning system
        let training_coordinator = Arc::new(TrainingCoordinator::new(
            100,  // buffer_size: keep last 100 examples
            10,   // threshold: train after 10 examples
            true, // auto_train: enabled
        ));

        let sampling_config = SamplingConfig::default(); // 5% baseline, 3x arch, 5x security
        let sampler = Arc::new(RwLock::new(Sampler::new(sampling_config)));

        // Only show LoRA log in standalone mode (daemon handles training)
        if is_interactive && !daemon_mode {
            output_status!("✓ LoRA fine-tuning enabled (weighted training)");
        }

        // Initialize TUI renderer if enabled (Phase 2: Ratatui interface)
        // Moved to global for Phase 5 native ratatui dialogs
        if config.tui_enabled && is_interactive {
            match TuiRenderer::new(
                Arc::new(output_manager.clone()),
                Arc::new(status_bar.clone()),
                config.colors.clone(),
            ) {
                Ok(renderer) => {
                    output_status!("✓ TUI mode enabled (Ratatui)");

                    // Enable buffering for TUI mode (fixes output regression)
                    output_manager.enable_buffering();

                    // Set global TUI renderer for Menu dialogs (Phase 5)
                    use crate::cli::global_output::set_global_tui_renderer;
                    set_global_tui_renderer(renderer);
                }
                Err(e) => {
                    output_status!("⚠️  Failed to initialize TUI: {}", e);
                    output_status!("   Falling back to standard output mode");
                }
            }
        }

        // Initialize TeacherSession with context optimization
        let teacher_config = TeacherContextConfig {
            max_context_turns: 15,           // Keep last 15 turns
            tool_result_retention_turns: 5,  // Keep last 5 turns of tool results
            prompt_caching_enabled: true,
        };

        // Store providers and teachers for runtime switching
        let available_providers = config.providers.clone();
        let available_teachers = config.teachers.clone();

        let teacher_provider = match crate::providers::create_provider(&config.teachers) {
            Ok(provider) => provider,
            Err(e) => {
                output_status!("⚠️  Failed to create teacher provider: {}", e);
                output_status!("   Using default Claude fallback");
                // This shouldn't happen since claude_client already works,
                // but we need to handle the error case
                panic!("Cannot create teacher provider")
            }
        };

        let teacher_session = Arc::new(RwLock::new(TeacherSession::with_config(
            teacher_provider,
            teacher_config,
        )));

        // Only show teacher optimization log in standalone mode
        if is_interactive && !daemon_mode {
            output_status!("✓ Teacher context optimization enabled (Level 3)");
        }

        // daemon_client is now passed as a parameter (checked in main.rs)
        // Show warning only if in standalone mode and interactive
        if daemon_client.is_none() && is_interactive && !daemon_mode {
            output_status!("⚠️  Daemon not available, using teacher API directly");
        }

        // Phase 1: Initialize conversation logger
        let log_path = dirs::home_dir()
            .map(|home| home.join(".finch").join("conversations.jsonl"))
            .unwrap_or_else(|| PathBuf::from(".finch/conversations.jsonl"));
        let conversation_logger = Arc::new(tokio::sync::Mutex::new(
            crate::logging::ConversationLogger::new(log_path)
                .unwrap_or_else(|e| {
                    output_status!("⚠️  Failed to create conversation logger: {}", e);
                    panic!("Cannot create conversation logger")
                })
        ));
        if is_interactive && !daemon_mode {
            output_status!("✓ Conversation logging enabled (for future LoRA training)");
        }

        // Phase 2: Load active persona
        let active_persona = match crate::config::Persona::load_builtin(&config.active_persona) {
            Ok(persona) => {
                if is_interactive && !daemon_mode {
                    output_status!("✓ Persona loaded: {}", persona.name());
                }
                Arc::new(RwLock::new(persona))
            }
            Err(e) => {
                output_status!("⚠️  Failed to load persona '{}': {}", config.active_persona, e);
                output_status!("   Using default persona");
                Arc::new(RwLock::new(crate::config::Persona::default()))
            }
        };

        Self {
            _config: config,
            claude_client,
            daemon_client,
            teacher_session,
            available_providers,
            available_teachers,
            active_provider_index: 0,
            active_teacher_index: 0, // First teacher is active by default
            router, // Contains ThresholdRouter now
            metrics_logger,
            threshold_validator,
            local_generator,
            training_trends: TrainingTrends::new(20), // Track last 20 queries
            models_dir,
            bootstrap_loader,
            tokenizer,
            tool_executor,
            tool_definitions,
            is_interactive,
            streaming_enabled,
            debug_enabled: false,
            input_handler,
            conversation: Arc::new(RwLock::new(ConversationHistory::new())),
            mode: ReplMode::Normal,
            // LoRA fine-tuning
            training_coordinator,
            sampler,
            last_query: None,
            last_response: None,
            last_was_sampled: false,
            // Output management (Phase 1: Terminal UI refactor)
            output_manager,
            status_bar,
            // TUI renderer moved to global (Phase 5: Native ratatui dialogs)

            // Phase 1 & 2: Multi-LLM + Persona
            llm_registry,
            conversation_logger,
            active_persona,

            // Phase 4: Hierarchical memory
            memory_system,
        }
    }

    /// Try to connect to daemon (non-blocking, returns None if not available)
    async fn try_connect_daemon(config: &Config) -> Option<Arc<crate::client::DaemonClient>> {
        use crate::client::{DaemonClient, DaemonConfig};

        // Only try if daemon is enabled in config
        if !config.client.use_daemon {
            return None;
        }

        // Try to connect with short timeout
        let daemon_config = DaemonConfig {
            bind_address: config.client.daemon_address.clone(),
            auto_spawn: config.client.auto_spawn,
            timeout_seconds: 5, // Short timeout for non-blocking check
        };

        match DaemonClient::connect(daemon_config).await {
            Ok(client) => Some(Arc::new(client)),
            Err(e) => {
                tracing::debug!("Failed to connect to daemon: {}", e);
                None
            }
        }
    }

    /// Call teacher with context optimization (helper for MessageRequest → ProviderRequest conversion)
    async fn call_teacher(&self, request: &MessageRequest) -> Result<crate::claude::types::MessageResponse> {
        use crate::providers::ProviderRequest;

        // Convert MessageRequest to ProviderRequest
        let provider_request = ProviderRequest {
            messages: request.messages.clone(),
            model: request.model.clone(),
            max_tokens: request.max_tokens,
            temperature: None,
            tools: request.tools.clone(),
            stream: false,
            system: request.system.clone(),
        };

        // Send with Level 3 optimization (smart strategies)
        let mut session = self.teacher_session.write().await;
        let response = session.send_message_with_optimization(&provider_request).await?;

        // Convert ProviderResponse back to MessageResponse
        Ok(response.into())
    }

    /// Call teacher with streaming and context optimization
    async fn call_teacher_stream(&self, request: &MessageRequest) -> Result<tokio::sync::mpsc::Receiver<Result<crate::providers::StreamChunk>>> {
        use crate::providers::ProviderRequest;

        // Convert MessageRequest to ProviderRequest
        let provider_request = ProviderRequest {
            messages: request.messages.clone(),
            model: request.model.clone(),
            max_tokens: request.max_tokens,
            temperature: None,
            tools: request.tools.clone(),
            stream: true,
            system: request.system.clone(),
        };

        // Send with streaming (Level 1 tracking only, no truncation for streaming)
        let mut session = self.teacher_session.write().await;
        session.send_message_stream(&provider_request).await
    }

    /// Load local generator from disk or create new one WITH neural models
    async fn load_local_generator_with_models(
        models_dir: Option<&PathBuf>,
        is_interactive: bool,
        batch_trainer: Arc<RwLock<BatchTrainer>>,
        _tokenizer: Arc<TextTokenizer>,
    ) -> crate::local::LocalGenerator {
        use crate::local::LocalGenerator;

        // Get neural models from batch trainer
        let neural_generator = {
            let trainer = batch_trainer.read().await;
            Some(trainer.generator())
        };

        // Try to load existing local generator state
        if let Some(models_dir) = models_dir {
            let generator_path = models_dir.join("local_generator.json");
            if generator_path.exists() {
                match LocalGenerator::load(&generator_path) {
                    Ok(_generator) => {
                        if is_interactive {
                            output_status!(
                                "✓ Loaded local generator from: {}",
                                generator_path.display()
                            );
                        }
                        // Note: loaded generator won't have neural models yet
                        // We'd need to refactor LocalGenerator to support injecting them
                        // For now, create fresh with models
                        return LocalGenerator::with_models(neural_generator);
                    }
                    Err(e) => {
                        if is_interactive {
                            output_status!("⚠️  Failed to load local generator: {}", e);
                            output_status!("   Starting with new generator");
                        }
                    }
                }
            }
        }

        // Create new with neural models
        LocalGenerator::with_models(neural_generator)
    }

    /// Load validator from disk or create new one
    fn load_validator(models_dir: Option<&PathBuf>, is_interactive: bool, daemon_mode: bool) -> ThresholdValidator {
        let Some(models_dir) = models_dir else {
            return ThresholdValidator::new();
        };

        let validator_path = models_dir.join("threshold_validator.json");
        if validator_path.exists() {
            match ThresholdValidator::load(&validator_path) {
                Ok(validator) => {
                    // Only show validator log in standalone mode (daemon has its own)
                    if is_interactive && !daemon_mode {
                        output_status!(
                            "✓ Loaded validator with {} validations",
                            validator.stats().total_validations
                        );
                    }
                    validator
                }
                Err(e) => {
                    if is_interactive {
                        output_status!("⚠️  Failed to load validator: {}", e);
                    }
                    ThresholdValidator::new()
                }
            }
        } else {
            ThresholdValidator::new()
        }
    }

    // ========================================================================
    // Output Management Methods (Phase 1: Terminal UI refactor)
    // These methods provide a unified interface for output, allowing us to
    // buffer messages for eventual TUI rendering while keeping dual output
    // to stdout during the transition.
    // ========================================================================

    /// Output a user message (dual: buffer + stdout, or TUI only)
    fn output_user(&self, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager.write_user(content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            println!("> {}", content_str);
        }
    }

    /// Output a provider response (dual: buffer + stdout, or TUI only)
    fn output_response(&self, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager.write_response(content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            println!("{}", content_str);
        }
    }

    /// Append to the last provider response (for streaming)
    fn output_response_append(&self, content: impl AsRef<str>) {
        let content_str = content.as_ref();
        self.output_manager.append_response(content_str);
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            print!("{}", content_str);
            let _ = io::stdout().flush();
        }
    }

    /// Output tool execution result (dual: buffer + stdout, or TUI only)
    fn output_tool(&self, tool_name: impl Into<String>, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager
            .write_tool(tool_name, content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            println!("{}", content_str);
        }
    }

    /// Output status information (dual: buffer + stdout, or TUI only)
    fn output_status(&self, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager.write_status(content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            eprintln!("{}", content_str);
        }
    }

    /// Output error message (dual: buffer + stdout, or TUI only)
    fn output_error(&self, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager.write_error(content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            eprintln!("{}", content_str);
        }
    }

    /// Update training statistics in status bar
    fn update_training_stats(&self, total_queries: usize, local_percentage: f64, quality: f64) {
        self.status_bar
            .update_training_stats(total_queries, local_percentage, quality);
    }

    /// Update download progress in status bar
    fn update_download_progress(
        &self,
        model_name: impl Into<String>,
        percentage: f64,
        downloaded: u64,
        total: u64,
    ) {
        self.status_bar
            .update_download_progress(model_name, percentage, downloaded, total);
    }

    /// Update operation status in status bar
    fn update_operation_status(&self, operation: impl Into<String>) {
        self.status_bar.update_operation(operation);
    }

    /// Clear operation status from status bar
    fn clear_operation_status(&self) {
        self.status_bar.clear_operation();
    }

    /// Check if TUI is active
    fn is_tui_active(&self) -> bool {
        use crate::cli::global_output::get_global_tui_renderer;
        let tui_renderer = get_global_tui_renderer();
        if let Ok(tui_lock) = tui_renderer.lock() {
            tui_lock.is_some()
        } else {
            false
        }
    }

    /// Render the TUI (if enabled)
    fn render_tui(&mut self) {
        use crate::cli::global_output::get_global_tui_renderer;
        let tui_renderer = get_global_tui_renderer();
        if let Ok(mut tui_lock) = tui_renderer.lock() {
            if let Some(ref mut tui) = *tui_lock {
                if let Err(e) = tui.render() {
                    self.output_error(format!("⚠️  TUI render error: {}", e));
                }
            }
        }
    }

    // ========================================================================
    // End Output Management Methods
    // ========================================================================

    /// Save models to disk
    async fn save_models(&mut self) -> Result<()> {
        let Some(ref models_dir) = self.models_dir else {
            // Still save patterns even if no models directory
            self.tool_executor.lock().await.save_patterns()?;
            return Ok(());
        };

        std::fs::create_dir_all(models_dir)?;

        if self.is_interactive {
            print!("{}", "Saving models and patterns... ".dark_grey());
            io::stdout().flush()?;
        }

        // Save router (includes threshold router)
        self.router.save(models_dir.join("threshold_router.json"))?;

        // Save validator separately
        self.threshold_validator
            .save(models_dir.join("threshold_validator.json"))?;

        // Save local generator
        {
            let gen = self.local_generator.read().await;
            gen.save(models_dir.join("local_generator.json"))?;
        }

        // Save tool patterns
        self.tool_executor.lock().await.save_patterns()?;

        if self.is_interactive {
            self.output_status("✓");
        }

        Ok(())
    }

    /// Execute tools and re-invoke Claude until no more tool uses
    async fn execute_tool_loop(
        &mut self,
        initial_response: crate::claude::MessageResponse,
    ) -> Result<String> {
        let mut current_response = initial_response;
        let mut iteration = 0;
        const MAX_ITERATIONS: u32 = 25; // Higher safety limit (prevents truly infinite loops)
        const MAX_CONSECUTIVE_SAME_TOOL: usize = 3; // Limit per tool

        // Track tool calls to detect infinite loops (signature-based)
        let mut tool_call_history: Vec<(String, String)> = Vec::new();

        // Track consecutive usage per tool
        let mut consecutive_tool_usage: HashMap<String, usize> = HashMap::new();

        while current_response.has_tool_uses() && iteration < MAX_ITERATIONS {
            iteration += 1;

            let tool_uses = current_response.tool_uses();

            if self.is_interactive {
                self.output_status(format!("🔧 Executing {} tool(s)...", tool_uses.len()));
            }

            // Check for excessive use of same tool (per-tool limit)
            for tool_use in &tool_uses {
                let count = consecutive_tool_usage.get(&tool_use.name).unwrap_or(&0);

                if *count >= MAX_CONSECUTIVE_SAME_TOOL {
                    let error_msg = format!(
                        "⚠️  Tool '{}' called {} times consecutively. Possible infinite loop detected.",
                        tool_use.name, count
                    );

                    if self.is_interactive {
                        self.output_error(&error_msg);
                        self.output_error("⚠️  Breaking to prevent infinite loop...");
                    }

                    // Add explanation to conversation
                    let explanation = format!(
                        "Tool execution stopped: Detected possible infinite loop. \
                         Tool '{}' was called {} times consecutively without switching to different tools.",
                        tool_use.name, count
                    );

                    return Ok(explanation);
                }
            }

            // Check for repeated tool calls (signature-based infinite loop detection)
            for tool_use in &tool_uses {
                let input_hash = format!("{:?}", tool_use.input);
                let signature = (tool_use.name.clone(), input_hash.clone());

                // Count how many times we've seen this exact tool call
                let repeat_count = tool_call_history
                    .iter()
                    .filter(|sig| *sig == &signature)
                    .count();

                if repeat_count >= 2 {
                    if self.is_interactive {
                        self.output_error(format!(
                            "⚠️  Warning: Tool '{}' called {} times with same input",
                            tool_use.name,
                            repeat_count + 1
                        ));
                        self.output_error("⚠️  Possible infinite loop detected. Breaking...");
                    }

                    // Add error message to conversation explaining the issue
                    let error_msg = format!(
                        "Tool execution stopped: Detected infinite loop. \
                         Tool '{}' was called {} times with the same input.",
                        tool_use.name,
                        repeat_count + 1
                    );

                    return Ok(error_msg);
                }

                tool_call_history.push(signature);
            }

            // Execute all tool uses
            let mut tool_results = Vec::new();
            for tool_use in &tool_uses {
                if self.is_interactive {
                    self.output_tool(&tool_use.name, format!("  → {}", tool_use.name));
                }

                // Check mode-based permissions first
                if !Self::is_tool_allowed_in_mode(&tool_use.name, &self.mode) {
                    use crate::tools::types::ToolResult;
                    let error_result = ToolResult::error(
                        tool_use.id.clone(),
                        format!(
                            "Tool '{}' is not allowed in planning mode.\n\
                             Reason: This tool can modify system state.\n\
                             Available tools: read, glob, grep, web_fetch\n\
                             Type /approve to execute your plan with all tools enabled.",
                            tool_use.name
                        ),
                    );
                    tool_results.push(error_result);
                    if self.is_interactive {
                        self.output_tool(&tool_use.name, "    ✗ Blocked by plan mode");
                    }
                    continue;
                }

                // Generate tool signature for approval checking
                let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let signature = generate_tool_signature(tool_use, &working_dir);

                // Auto-approve certain non-destructive operations
                let is_auto_approved = {
                    let tool_name = tool_use.name.as_str();

                    // Always auto-approve EnterPlanMode (non-destructive mode change)
                    if tool_name == "EnterPlanMode" || tool_name == "enter_plan_mode" {
                        true
                    } else {
                        // Auto-approve read-only tools and user interaction tools when in plan mode
                        let is_plan_mode = matches!(self.mode, ReplMode::Planning { .. });
                        let is_readonly_tool = matches!(
                            tool_name,
                            "read" | "Read" | "glob" | "Glob" | "grep" | "Grep" | "web_fetch" | "WebFetch" |
                            "AskUserQuestion" | "ask_user_question"
                        );

                        is_plan_mode && is_readonly_tool
                    }
                };

                // Check if pre-approved in cache
                let approval_source = self.tool_executor.lock().await.is_approved(&signature);

                match approval_source {
                    ApprovalSource::NotApproved if !is_auto_approved => {
                        // Show prompt if interactive
                        if self.is_interactive {
                            match self.confirm_tool_execution(tool_use, &signature)? {
                                ConfirmationResult::ApproveOnce => {
                                    self.output_tool(&tool_use.name, "  ✓ Approved");
                                }
                                ConfirmationResult::ApproveExactSession(sig) => {
                                    self.tool_executor.lock().await.approve_exact_session(sig);
                                    self.output_tool(&tool_use.name, "  ✓ Approved (remembered for session)");
                                }
                                ConfirmationResult::ApprovePatternSession(pattern) => {
                                    self.output_tool(&tool_use.name, format!("  ✓ Approved pattern: {} (session)", pattern.pattern));
                                    self.tool_executor.lock().await.approve_pattern_session(pattern);
                                }
                                ConfirmationResult::ApproveExactPersistent(sig) => {
                                    self.tool_executor.lock().await.approve_exact_persistent(sig);
                                    // IMMEDIATE SAVE: Don't wait for checkpoint
                                    if let Err(e) = self.tool_executor.lock().await.save_patterns() {
                                        self.output_status(format!("  ⚠️  Warning: Failed to save pattern: {}", e));
                                        self.output_tool(&tool_use.name, "  ✓ Approved (this session only - save failed)");
                                    } else {
                                        self.output_tool(&tool_use.name, "  ✓ Approved (saved permanently)");
                                    }
                                }
                                ConfirmationResult::ApprovePatternPersistent(pattern) => {
                                    let pattern_str = pattern.pattern.clone();
                                    self.tool_executor.lock().await.approve_pattern_persistent(pattern);
                                    // IMMEDIATE SAVE: Don't wait for checkpoint
                                    if let Err(e) = self.tool_executor.lock().await.save_patterns() {
                                        self.output_status(format!("  ⚠️  Warning: Failed to save pattern: {}", e));
                                        self.output_tool(&tool_use.name, format!(
                                            "  ✓ Approved pattern: {} (this session only - save failed)",
                                            pattern_str
                                        ));
                                    } else {
                                        self.output_tool(&tool_use.name, format!(
                                            "  ✓ Approved pattern: {} (saved permanently)",
                                            pattern_str
                                        ));
                                    }
                                }
                                ConfirmationResult::Deny => {
                                    use crate::tools::types::ToolResult;
                                    let error_result = ToolResult::error(
                                        tool_use.id.clone(),
                                        "Tool execution denied by user".to_string(),
                                    );
                                    tool_results.push(error_result);
                                    self.output_tool(&tool_use.name, "    ✗ Denied by user");
                                    continue;
                                }
                            }
                        }
                    }
                    ApprovalSource::NotApproved => {
                        // Auto-approved (non-destructive operation)
                        // Continue to execution without prompting
                    }
                    ApprovalSource::SessionExact => {
                        // Already approved, execute silently
                    }
                    ApprovalSource::SessionPattern(ref id) => {
                        if self.is_interactive {
                            self.output_tool(&tool_use.name, format!("  ✓ Matched session pattern ({})", &id[..8]));
                        }
                    }
                    ApprovalSource::PersistentExact => {
                        if self.is_interactive {
                            self.output_tool(&tool_use.name, "  ✓ Matched saved approval");
                        }
                    }
                    ApprovalSource::PersistentPattern(ref id) => {
                        if self.is_interactive {
                            self.output_tool(&tool_use.name, format!("  ✓ Matched saved pattern ({})", &id[..8]));
                        }
                    }
                }

                // Create save function that captures necessary state
                let models_dir = self.models_dir.clone();
                let router_ref = &self.router;
                let validator_ref = &self.threshold_validator;
                let save_fn = || -> Result<()> {
                    if let Some(ref dir) = models_dir {
                        std::fs::create_dir_all(dir)?;
                        router_ref.save(dir.join("threshold_router.json"))?;
                        validator_ref.save(dir.join("threshold_validator.json"))?;
                    }
                    Ok(())
                };

                let conversation_snapshot = self.conversation.read().await.clone();
                let result = self
                    .tool_executor
                    .lock()
                    .await
                    .execute_tool(
                        tool_use,
                        Some(&conversation_snapshot),
                        Some(save_fn),
                        None, // TODO: Add training via BootstrapLoader's generator
                        Some(Arc::clone(&self.local_generator)),
                        Some(Arc::clone(&self.tokenizer)),
                        None, // repl_mode (not available in raw mode)
                        None, // plan_content
                    )
                    .await?;

                // Display tool result to user (Phase 1: Visibility)
                if self.is_interactive {
                    if result.is_error {
                        self.output_tool(&tool_use.name, format!("    ✗ Error: {}", result.content));
                    } else {
                        self.output_tool(&tool_use.name, "    ✓ Success");

                        // Show preview of result (first 500 chars)
                        let preview = if result.content.len() > 500 {
                            format!(
                                "{}... [truncated, {} chars total]",
                                &result.content[..500],
                                result.content.len()
                            )
                        } else {
                            result.content.clone()
                        };

                        // Indent output for readability
                        for line in preview.lines() {
                            self.output_tool(&tool_use.name, format!("      {}", line));
                        }
                    }
                    self.output_status(""); // Blank line after each tool
                }

                tool_results.push(result);
            }

            // Update consecutive tool usage counters
            let current_tool_names: HashSet<_> = tool_uses.iter().map(|t| t.name.clone()).collect();

            // Increment counters for tools used in this iteration
            for tool_use in &tool_uses {
                *consecutive_tool_usage
                    .entry(tool_use.name.clone())
                    .or_insert(0) += 1;
            }

            // Reset counters for tools NOT in current execution (user switched tools)
            consecutive_tool_usage.retain(|name, _| current_tool_names.contains(name));

            // Build tool result message for Claude using XML-like structure
            // This format is easier for Claude to parse
            let mut tool_result_text = String::new();
            for (idx, result) in tool_results.iter().enumerate() {
                let tool_name = tool_uses[idx].name.as_str();

                if result.is_error {
                    tool_result_text.push_str(&format!(
                        "<tool_result>\n\
                         <tool_name>{}</tool_name>\n\
                         <status>error</status>\n\
                         <content>{}</content>\n\
                         </tool_result>\n\n",
                        tool_name, result.content
                    ));
                } else {
                    tool_result_text.push_str(&format!(
                        "<tool_result>\n\
                         <tool_name>{}</tool_name>\n\
                         <status>success</status>\n\
                         <content>{}</content>\n\
                         </tool_result>\n\n",
                        tool_name, result.content
                    ));
                }
            }

            // Important: Add Claude's tool-use response to conversation first
            // This maintains the user/assistant alternation required by the API
            let assistant_text = current_response.text();

            if assistant_text.is_empty() {
                // Response contains ONLY tool_use blocks, no text
                // We MUST add something to maintain conversation alternation
                self.conversation.write().await.add_assistant_message("[Tool request]".to_string());

                if self.is_interactive {
                    self.output_status("    (Claude requesting tool execution)");
                }
            } else {
                // Response has both text and tool_use blocks
                self.conversation.write().await.add_assistant_message(assistant_text.clone());

                if self.is_interactive && !assistant_text.trim().is_empty() {
                    self.output_response(format!("    Claude: {}", assistant_text));
                }
            }

            // Then add tool results as a user message
            self.conversation.write().await.add_user_message(tool_result_text);

            // Re-invoke teacher with tool results (using optimized context)
            let request = MessageRequest::with_context(self.conversation.read().await.get_messages())
                .with_tools(self.tool_definitions.clone());

            current_response = self.call_teacher(&request).await?;
        }

        // Handle max iterations or completion
        if iteration >= MAX_ITERATIONS {
            if self.is_interactive {
                self.output_error(format!(
                    "⚠️  Warning: Max tool iterations reached ({})",
                    MAX_ITERATIONS
                ));
                self.output_error("⚠️  Claude may be stuck in a loop. Returning last response.");
            }

            // IMPORTANT: Still add final response to conversation
            // Even if we hit max iterations, we need to maintain conversation state
            let final_text = current_response.text();
            if !final_text.is_empty() {
                self.conversation.write().await.add_assistant_message(final_text.clone());
            }
        }

        // Validate conversation state (debug check)
        let messages = self.conversation.read().await.get_messages();
        if messages.is_empty() {
            self.output_error("⚠️  ERROR: Conversation became empty after tool loop!");
            self.output_error("⚠️  This is a bug - please report to developers");
        }

        // Check for empty messages
        for (i, msg) in messages.iter().enumerate() {
            if msg.is_empty_text() {
                self.output_error(format!("⚠️  WARNING: Message {} has empty content", i));
            }
        }

        Ok(current_response.text())
    }

    /// Display streaming response character-by-character (handles new StreamChunk format)
    async fn display_streaming_response(
        &mut self,
        mut rx: mpsc::Receiver<Result<crate::generators::StreamChunk>>,
    ) -> Result<String> {
        let mut full_response = String::new();

        // Print newline to start response area
        if self.is_interactive {
            self.output_status("");
        }

        // Start a new provider response in the output buffer
        if full_response.is_empty() {
            self.output_manager.write_response(String::new());
        }

        while let Some(result) = rx.recv().await {
            match result {
                Ok(crate::generators::StreamChunk::TextDelta(text_chunk)) => {
                    full_response.push_str(&text_chunk);

                    // Update the output buffer with the chunk
                    self.output_manager.append_response(text_chunk.clone());

                    // Render TUI to show the update (if active)
                    if self.is_tui_active() {
                        self.render_tui();
                    } else {
                        // Fallback: print chunk directly if no TUI
                        use std::io::Write;
                        print!("{}", text_chunk);
                        let _ = std::io::stdout().flush();
                    }
                }
                Ok(crate::generators::StreamChunk::ContentBlockComplete(_block)) => {
                    // Tool block completed - ignore for now (event loop handles tools)
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        // Final newline after response
        if self.is_interactive {
            self.output_status("");
        }

        Ok(full_response)
    }

    /// Restore conversation from a saved state
    pub fn restore_conversation(&mut self, history: ConversationHistory) {
        self.conversation = Arc::new(RwLock::new(history));
    }

    /// Run REPL with an optional initial prompt
    pub async fn run_with_initial_prompt(&mut self, initial_prompt: Option<String>) -> Result<()> {
        if let Some(prompt) = initial_prompt {
            // Process initial prompt before starting interactive loop
            if self.is_interactive {
                self.output_status(format!("\nProcessing initial prompt: \"{}\"", prompt));
                self.output_status("");
            }
            let response = self.process_query(&prompt).await?;
            if self.is_interactive {
                self.output_response(&response);
            }
        }

        // Continue with normal REPL loop
        self.run().await
    }

    /// Run REPL in event loop mode (concurrent queries and tools)
    /// Falls back to traditional mode if TUI is not available
    pub async fn run_event_loop(&mut self, initial_prompt: Option<String>) -> Result<()> {
        use crate::cli::global_output::get_global_tui_renderer;
        use crate::cli::repl_event::EventLoop;

        // Check if TUI is available (required for event loop)
        let tui_renderer = {
            let mut tui_lock = get_global_tui_renderer().lock().unwrap();
            match tui_lock.take() {
                Some(renderer) => renderer,
                None => {
                    // TUI not available (raw mode), fall back to traditional REPL
                    return self.run_with_initial_prompt(initial_prompt).await;
                }
            }
        };

        // Event loop is always enabled in interactive mode - no need to announce
        // if self.is_interactive {
        //     self.output_status("✓ Event loop mode enabled (concurrent execution)");
        // }

        // Create generators
        use crate::generators::{claude::ClaudeGenerator, qwen::QwenGenerator};
        let claude_gen: Arc<dyn crate::generators::Generator> =
            Arc::new(ClaudeGenerator::new(Arc::new(self.claude_client.clone())));
        let qwen_gen: Arc<dyn crate::generators::Generator> = Arc::new(QwenGenerator::new(
            Arc::clone(&self.local_generator),
            Arc::clone(&self.tokenizer),
            Some(Arc::clone(&self.tool_executor)), // Enable tool support
        ));

        // Get generator state from bootstrap loader
        let generator_state = Arc::clone(self.bootstrap_loader.state());

        // Extract mode to Arc (replace with Normal temporarily)
        let mode = Arc::new(RwLock::new(std::mem::replace(&mut self.mode, ReplMode::Normal)));

        // Create EventLoop with all dependencies
        let mut event_loop = EventLoop::new(
            Arc::clone(&self.conversation),
            claude_gen,
            qwen_gen,
            Arc::new(self.router.clone()),
            generator_state,
            self.tool_definitions.clone(),
            Arc::clone(&self.tool_executor),
            tui_renderer,
            Arc::new(self.output_manager.clone()),
            Arc::new(self.status_bar.clone()),
            self.streaming_enabled,
            Arc::clone(&self.local_generator),
            Arc::clone(&self.tokenizer),
            self.daemon_client.clone(),
            mode,
            None, // memory_tree - will be added in future when memory system is integrated
        );

        // Run the event loop
        event_loop.run().await
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.is_interactive {
            // Fancy startup for interactive mode
            self.output_status("Shammah v0.1.0 - Constitutional AI Proxy");
            self.output_status("Using API key from: ~/.finch/config.toml ✓");
            self.output_status("Loaded crisis detection keywords ✓");
            self.output_status("Online learning: ENABLED (threshold models) ✓");
            self.output_status("");
            self.output_status("Ready. Type /help for commands.");
            self.print_status_line().await;
        } else {
            // Minimal output for non-interactive mode (pipes, scripts)
            output_status!("# Shammah v0.1.0 - Non-interactive mode");
        }

        // Register Ctrl+C handler for graceful shutdown
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = shutdown_flag.clone();

        ctrlc::set_handler(move || {
            let already_requested = flag_clone.swap(true, Ordering::SeqCst);
            if already_requested {
                // Second Ctrl+C: Force exit immediately
                eprintln!("\n⚠️  Force exit - cleaning up terminal...");

                // Emergency terminal cleanup
                let _ = crossterm::terminal::disable_raw_mode();
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::cursor::Show,
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                );

                std::process::exit(130); // Standard exit code for SIGINT
            }
        })?;

        loop {
            // Check for shutdown (Ctrl+C)
            if shutdown_flag.load(Ordering::SeqCst) {
                eprintln!("[DEBUG] Shutdown flag detected");

                // CRITICAL: Disable raw mode FIRST so Ctrl+C can work if shutdown hangs
                eprintln!("[DEBUG] Disabling raw mode...");
                let _ = crossterm::terminal::disable_raw_mode();

                if self.is_interactive {
                    eprintln!();
                    eprintln!("^C interrupt - shutting down...");
                    eprintln!("(Press Ctrl+C again to force quit)");
                }

                eprintln!("[DEBUG] Saving models...");
                self.save_models().await?;

                if self.is_interactive {
                    eprintln!("Models saved. Goodbye!");
                }

                // Shutdown TUI before exiting (in case Ctrl+C was pressed during processing)
                if self.is_tui_active() {
                    eprintln!("[DEBUG] Shutting down TUI...");
                    use crate::cli::global_output::shutdown_global_tui;
                    let _ = shutdown_global_tui();
                    eprintln!("[DEBUG] TUI shutdown returned");
                }
                eprintln!("[DEBUG] Breaking from main loop");
                break;
            }

            // Read input using TUI integrated input, readline, or fallback
            let input = if self.is_tui_active() {
                // TUI MODE: use integrated tui-textarea input
                use crate::cli::global_output::get_global_tui_renderer;
                let tui_renderer = get_global_tui_renderer();

                let line = if let Ok(mut tui_lock) = tui_renderer.lock() {
                    if let Some(ref mut tui) = *tui_lock {
                        match tui.read_line()? {
                            Some(text) => Some(text),
                            None => {
                                // Ctrl+C or Esc - exit gracefully

                                // CRITICAL: Disable raw mode FIRST so Ctrl+C works if shutdown hangs
                                drop(tui_lock); // Release lock BEFORE disabling raw mode
                                let _ = crossterm::terminal::disable_raw_mode();

                                eprintln!();
                                eprintln!("Shutting down gracefully...");
                                eprintln!("(Press Ctrl+C again to force quit if it hangs)");

                                self.save_models().await?;
                                eprintln!("Models saved.");

                                // Shutdown TUI
                                use crate::cli::global_output::shutdown_global_tui;
                                let _ = shutdown_global_tui();
                                eprintln!("Goodbye!");
                                break;
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                match line {
                    Some(text) => text,
                    None => continue, // Fallback if TUI lock fails
                }
            } else if self.input_handler.is_some() {
                // RAW MODE: use rustyline (traditional CLI)
                if self.is_interactive {
                    self.output_status("");
                    self.print_separator();
                }

                let prompt = self.get_prompt();
                let handler = self.input_handler.as_mut().expect("input_handler is Some — guarded by is_some() above");
                let line = handler.read_line(&prompt)?;

                match line {
                    Some(text) => text,
                    None => {
                        // Ctrl+C or Ctrl+D - graceful exit
                        self.output_status("");
                        self.save_models().await?;
                        if let Some(ref mut handler) = self.input_handler {
                            if let Err(e) = handler.save_history() {
                                self.output_status(format!("⚠️  Failed to save history: {}", e));
                            }
                        }
                        self.output_status("Models saved. Goodbye!");
                        break;
                    }
                }
            } else {
                // Fallback: basic stdin reading (non-interactive or readline failed)
                if self.is_interactive {
                    self.output_status("");
                    self.print_separator();
                    // Note: Can't use colored prompts in basic mode, so use plain text
                    let prompt = match &self.mode {
                        ReplMode::Normal => "> ",
                        ReplMode::Planning { .. } => "plan> ",
                        ReplMode::Executing { .. } => "exec> ",
                    };
                    print!("{}", prompt);
                } else {
                    print!("Query: ");
                }
                io::stdout().flush()?;

                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_err() {
                    break;
                }
                input.trim().to_string()
            };

            if input.is_empty() {
                continue;
            }

            if self.is_interactive {
                self.print_separator();
                self.output_status("");
            }

            // Check for slash commands
            if let Some(command) = Command::parse(&input) {
                match command {
                    Command::Quit => {
                        self.save_models().await?;
                        if let Some(ref mut handler) = self.input_handler {
                            if let Err(e) = handler.save_history() {
                                self.output_status(format!("⚠️  Failed to save history: {}", e));
                            }
                        }
                        if self.is_interactive {
                            self.output_status("Models saved. Goodbye!");
                        }
                        break;
                    }
                    Command::Clear => {
                        self.conversation.write().await.clear();
                        self.output_status("Conversation history cleared. Starting fresh.");
                        if self.is_interactive {
                            self.output_status("");
                            self.print_status_line().await;
                        }
                        continue;
                    }
                    Command::Compact(ref instruction) => {
                        self.handle_compact_command(instruction.clone()).await?;
                        continue;
                    }
                    Command::PatternsList => {
                        let output = self.list_patterns().await?;
                        self.output_status(output);
                        continue;
                    }
                    Command::PatternsRemove(ref id) => {
                        let output = self.remove_pattern(id).await?;
                        self.output_status(output);
                        continue;
                    }
                    Command::PatternsClear => {
                        let output = self.clear_patterns().await?;
                        self.output_status(output);
                        continue;
                    }
                    Command::PatternsAdd => {
                        let output = self.add_pattern_interactive().await?;
                        self.output_status(output);
                        continue;
                    }
                    Command::Plan(ref task) => {
                        self.handle_plan_command(task.clone()).await?;
                        continue;
                    }
                    // Feedback commands for weighted LoRA training
                    Command::FeedbackCritical(ref note) => {
                        self.handle_feedback(10.0, note.clone()).await?;
                        continue;
                    }
                    Command::FeedbackMedium(ref note) => {
                        self.handle_feedback(3.0, note.clone()).await?;
                        continue;
                    }
                    Command::FeedbackGood(ref note) => {
                        self.handle_feedback(1.0, note.clone()).await?;
                        continue;
                    }
                    // Local model query command
                    Command::Local { ref query } => {
                        self.handle_local_query(query).await?;
                        continue;
                    }
                    // Phase 2: Persona commands
                    Command::PersonaList => {
                        self.handle_persona_list().await?;
                        continue;
                    }
                    Command::PersonaSelect(ref name) => {
                        self.handle_persona_select(name).await?;
                        continue;
                    }
                    Command::PersonaShow => {
                        self.handle_persona_show().await?;
                        continue;
                    }
                    // Model/Teacher switching
                    Command::ModelList => {
                        self.handle_model_list().await?;
                        continue;
                    }
                    Command::ModelSwitch(ref name) => {
                        self.handle_model_switch(name).await?;
                        continue;
                    }
                    Command::ModelShow => {
                        self.handle_model_show().await?;
                        continue;
                    }
                    // Phase 3: Service discovery
                    Command::Discover => {
                        self.handle_discover().await?;
                        continue;
                    }
                    // Phase 4: Memory system
                    Command::Memory => {
                        self.handle_memory_stats().await?;
                        continue;
                    }
                    _ => {
                        let output = handle_command(
                            command,
                            &self.metrics_logger,
                            Some(&self.router), // CHANGED: pass router instead of threshold_router
                            Some(&self.threshold_validator),
                            &mut self.debug_enabled,
                        )?;
                        match output {
                            CommandOutput::Status(msg) | CommandOutput::Message(msg) => {
                                self.output_status(msg);
                            }
                        }
                        continue;
                    }
                }
            }

            // Process query
            match self.process_query(&input).await {
                Ok(response) => {
                    self.output_response(&response);
                    if self.is_interactive {
                        self.output_status("");
                        self.print_status_line().await;
                    }
                }
                Err(e) => {
                    self.output_error(format!("Error: {}", e));
                    if self.is_interactive {
                        self.output_status("");
                        self.print_status_line().await;
                    }
                }
            }
        }

        eprintln!("[DEBUG] After main loop - final cleanup");

        // Before exiting REPL, save any pending patterns
        if let Err(e) = self.save_models().await {
            eprintln!("⚠️  Failed to save on exit: {}", e);
        }

        eprintln!("[DEBUG] Final save complete, disabling raw mode");
        // Disable raw mode first so terminal is responsive
        let _ = crossterm::terminal::disable_raw_mode();

        // Shutdown TUI and restore terminal state (if not already done)
        // This is a safety net - TUI should already be shut down in the break paths above
        if self.is_tui_active() {
            eprintln!("[DEBUG] Final TUI shutdown (safety net)");
            use crate::cli::global_output::shutdown_global_tui;

            // shutdown_global_tui now has its own timeout/retry logic
            if let Err(e) = shutdown_global_tui() {
                eprintln!("Warning: Failed to shutdown TUI: {}", e);
            }
        }

        eprintln!("[DEBUG] Exiting run() function");
        Ok(())
    }

    /// Print separator line that adapts to terminal width
    fn print_separator(&self) {
        let width = terminal_width();
        self.output_status("─".repeat(width));
    }

    /// Prompt user to confirm tool execution
    fn confirm_tool_execution(
        &mut self,
        tool_use: &ToolUse,
        signature: &ToolSignature,
    ) -> Result<ConfirmationResult> {
        // Display tool information
        self.output_status("");
        self.output_status("Tool Execution Request:");
        self.output_status("─────────────────────────");
        self.output_status(format!("Tool: {}", tool_use.name));

        // Show relevant parameters
        self.display_tool_params(tool_use);

        self.output_status("");

        // Build menu options
        let options = vec![
            MenuOption::with_description(
                "Yes (once only)",
                "Execute this time, ask again next time",
                ConfirmationChoice::ApproveOnce,
            ),
            MenuOption::with_description(
                "Yes, and remember exact command for this session",
                "Won't ask again for this exact command in this session",
                ConfirmationChoice::ApproveExactSession,
            ),
            MenuOption::with_description(
                "Yes, and remember pattern for this session",
                "Won't ask again for similar commands in this session",
                ConfirmationChoice::ApprovePatternSession,
            ),
            MenuOption::with_description(
                "Yes, and ALWAYS allow this exact command",
                "Save permanently - never ask again for this exact command",
                ConfirmationChoice::ApproveExactPersistent,
            ),
            MenuOption::with_description(
                "Yes, and ALWAYS allow this pattern",
                "Save permanently - never ask again for similar commands",
                ConfirmationChoice::ApprovePatternPersistent,
            ),
            MenuOption::with_description(
                "No (deny)",
                "Block this tool execution",
                ConfirmationChoice::Deny,
            ),
        ];

        // Show menu
        let choice = Menu::select(
            "Do you want to proceed?",
            options,
            Some("[↑↓ or j/k to move, Enter to select, or type 1-6]"),
        )?;

        // Convert choice to ConfirmationResult
        match choice {
            ConfirmationChoice::ApproveOnce => Ok(ConfirmationResult::ApproveOnce),
            ConfirmationChoice::ApproveExactSession => {
                Ok(ConfirmationResult::ApproveExactSession(signature.clone()))
            }
            ConfirmationChoice::ApprovePatternSession => {
                let pattern = self.build_pattern_from_signature(signature)?;
                Ok(ConfirmationResult::ApprovePatternSession(pattern))
            }
            ConfirmationChoice::ApproveExactPersistent => Ok(
                ConfirmationResult::ApproveExactPersistent(signature.clone()),
            ),
            ConfirmationChoice::ApprovePatternPersistent => {
                let pattern = self.build_pattern_from_signature(signature)?;
                Ok(ConfirmationResult::ApprovePatternPersistent(pattern))
            }
            ConfirmationChoice::Deny => Ok(ConfirmationResult::Deny),
        }
    }

    /// Helper to read a single line choice
    fn read_choice(&mut self, prompt: &str) -> Result<Option<String>> {
        if let Some(ref mut handler) = self.input_handler {
            handler.read_line(prompt)
        } else {
            print!("{}", prompt);
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            Ok(Some(line.trim().to_string()))
        }
    }

    /// Build a pattern from a signature by prompting user for wildcard choices
    fn build_pattern_from_signature(&mut self, signature: &ToolSignature) -> Result<ToolPattern> {
        // Parse signature to extract components
        let (command_part, dir_part) = self.parse_signature_components(signature);

        self.output_status("");

        // Generate options based on tool type
        let pattern_options = if let (Some(ref cmd), Some(ref dir)) = (&command_part, &dir_part) {
            let base_cmd = cmd.split_whitespace().next().unwrap_or(cmd);
            vec![
                format!("{} * in {}", base_cmd, dir),
                format!("{} in *", cmd),
                format!("{} * in *", base_cmd),
            ]
        } else if signature.tool_name == "read" {
            // For read tool, offer path-based patterns
            vec![
                signature.context_key.clone(), // Exact file
                format!("{}/**", Self::get_dir_from_context(&signature.context_key)),
                format!("reading *"),
            ]
        } else {
            // Generic patterns
            vec![
                signature.context_key.clone(),
                format!("{} *", signature.tool_name),
            ]
        };

        // Build menu options with None for "Other" choice
        let mut menu_options: Vec<MenuOption<Option<String>>> = pattern_options
            .into_iter()
            .map(|p| MenuOption::new(p.clone(), Some(p)))
            .collect();

        // Add "Other" option for custom pattern
        menu_options.push(MenuOption::with_description(
            "✏️  Other (type custom pattern)",
            "Enter your own pattern",
            None,
        ));

        let selection = Menu::select(
            "What should the pattern match?",
            menu_options,
            Some("Choose a pattern or select Other to type your own"),
        )?;

        let pattern_str = match selection {
            Some(pattern) => pattern,
            None => {
                // User selected "Other" - prompt for custom pattern
                Menu::text_input(
                    "Enter custom pattern:",
                    None,
                    Some("Use * for wildcards, ** for recursive"),
                )?
            }
        };

        Ok(ToolPattern::new(
            pattern_str.clone(),
            signature.tool_name.clone(),
            format!("Pattern: {}", pattern_str),
        ))
    }

    /// Parse signature context_key into command and directory components
    fn parse_signature_components(
        &self,
        signature: &ToolSignature,
    ) -> (Option<String>, Option<String>) {
        match signature.tool_name.as_str() {
            "bash" | "save_and_exec" => {
                // Format: "command args in /dir"
                if let Some(pos) = signature.context_key.rfind(" in ") {
                    let command = signature.context_key[..pos].to_string();
                    let dir = signature.context_key[pos + 4..].to_string();
                    (Some(command), Some(dir))
                } else {
                    (Some(signature.context_key.clone()), None)
                }
            }
            "read" => {
                // Format: "reading /path/to/file"
                if let Some(pos) = signature.context_key.find(' ') {
                    let path = signature.context_key[pos + 1..].to_string();
                    (None, Some(path))
                } else {
                    (None, Some(signature.context_key.clone()))
                }
            }
            "grep" => {
                // Format: "pattern 'text' in /dir"
                if let Some(pos) = signature.context_key.rfind(" in ") {
                    let pattern = signature.context_key[..pos].to_string();
                    let dir = signature.context_key[pos + 4..].to_string();
                    (Some(pattern), Some(dir))
                } else {
                    (Some(signature.context_key.clone()), None)
                }
            }
            _ => (None, None),
        }
    }

    /// Extract directory from a context string
    /// Check if tool is allowed in current mode
    fn is_tool_allowed_in_mode(tool_name: &str, mode: &ReplMode) -> bool {
        match mode {
            ReplMode::Normal | ReplMode::Executing { .. } => {
                // All tools allowed (subject to normal confirmation)
                true
            }
            ReplMode::Planning { .. } => {
                // Only inspection tools allowed
                matches!(tool_name, "read" | "glob" | "grep" | "web_fetch")
            }
        }
    }

    fn get_dir_from_context(context: &str) -> String {
        // For "reading /path/to/file.txt", return "/path/to"
        if let Some(last_slash) = context.rfind('/') {
            if last_slash > 0 {
                return context[..last_slash].to_string();
            }
        }
        ".".to_string()
    }

    /// Display tool parameters in user-friendly format
    fn display_tool_params(&self, tool_use: &ToolUse) {
        match tool_use.name.as_str() {
            "bash" => {
                if let Some(command) = tool_use.input["command"].as_str() {
                    self.output_status(format!("  Command: {}", command));
                }
                if let Some(desc) = tool_use.input.get("description").and_then(|v| v.as_str()) {
                    self.output_status(format!("  Description: {}", desc));
                }
            }
            "read" => {
                if let Some(path) = tool_use.input["file_path"].as_str() {
                    self.output_status(format!("  File: {}", path));
                }
            }
            "web_fetch" => {
                if let Some(url) = tool_use.input["url"].as_str() {
                    self.output_status(format!("  URL: {}", url));
                }
                if let Some(prompt) = tool_use.input.get("prompt").and_then(|v| v.as_str()) {
                    self.output_status(format!("  Prompt: {}", prompt));
                }
            }
            "grep" => {
                if let Some(pattern) = tool_use.input["pattern"].as_str() {
                    self.output_status(format!("  Pattern: {}", pattern));
                }
                if let Some(path) = tool_use.input.get("path").and_then(|v| v.as_str()) {
                    self.output_status(format!("  Path: {}", path));
                }
            }
            "glob" => {
                if let Some(pattern) = tool_use.input["pattern"].as_str() {
                    self.output_status(format!("  Pattern: {}", pattern));
                }
            }
            "save_and_exec" => {
                if let Some(command) = tool_use.input["command"].as_str() {
                    self.output_status(format!("  Command: {}", command));
                }
                if let Some(reason) = tool_use.input.get("reason").and_then(|v| v.as_str()) {
                    self.output_status(format!("  Reason: {}", reason));
                }
            }
            _ => {
                // Generic display for unknown tools
                self.output_status(format!("  Input: {}", tool_use.input));
            }
        }
    }

    // ============================================================================
    // Pattern Management Methods (Phase 3)
    // ============================================================================

    /// Format a duration as a human-readable string
    fn format_duration(duration: chrono::Duration) -> String {
        if duration.num_seconds() < 60 {
            format!("{}s ago", duration.num_seconds())
        } else if duration.num_minutes() < 60 {
            format!("{}m ago", duration.num_minutes())
        } else if duration.num_hours() < 24 {
            format!("{}h ago", duration.num_hours())
        } else {
            format!("{}d ago", duration.num_days())
        }
    }

    /// List all confirmation patterns
    pub async fn list_patterns(&self) -> Result<String> {
        let executor = self.tool_executor.lock().await;
        let store = executor.persistent_store();
        let mut output = String::new();

        output.push_str("Confirmation Patterns\n");
        output.push_str("=====================\n\n");

        // Show patterns
        if store.patterns.is_empty() {
            output.push_str("No patterns configured.\n\n");
        } else {
            output.push_str("Patterns:\n");
            for pattern in &store.patterns {
                let now = chrono::Utc::now();
                let last_used_str = if let Some(last_used) = pattern.last_used {
                    let duration = now - last_used;
                    Self::format_duration(duration)
                } else {
                    "never".to_string()
                };

                let type_str = match pattern.pattern_type {
                    crate::tools::patterns::PatternType::Wildcard => "wildcard",
                    crate::tools::patterns::PatternType::Regex => "regex",
                    crate::tools::patterns::PatternType::Structured => "structured",
                };
                output.push_str(&format!(
                    "  {} ({})\n",
                    &pattern.id[..8.min(pattern.id.len())],
                    type_str
                ));
                output.push_str(&format!("    Tool: {}\n", pattern.tool_name));
                output.push_str(&format!("    Pattern: {}\n", pattern.pattern));
                output.push_str(&format!("    Description: {}\n", pattern.description));
                output.push_str(&format!(
                    "    Matches: {} | Last used: {}\n",
                    pattern.match_count, last_used_str
                ));
                output.push_str("\n");
            }
        }

        // Show exact approvals
        if store.exact_approvals.is_empty() {
            output.push_str("No exact approvals configured.\n");
        } else {
            output.push_str("Exact Approvals:\n");
            for approval in &store.exact_approvals {
                output.push_str(&format!(
                    "  {} ({})\n",
                    &approval.id[..8.min(approval.id.len())],
                    approval.tool_name
                ));
                output.push_str(&format!("    Signature: {}\n", approval.signature));
                output.push_str(&format!("    Matches: {}\n", approval.match_count));
                output.push_str("\n");
            }
        }

        output.push_str(&format!(
            "Total: {} patterns, {} exact approvals\n",
            store.patterns.len(),
            store.exact_approvals.len()
        ));

        Ok(output)
    }

    /// Remove a pattern by ID (supports partial matching with 8+ chars)
    pub async fn remove_pattern(&mut self, id: &str) -> Result<String> {
        let executor = self.tool_executor.lock().await;
        let store = executor.persistent_store();

        // Find pattern by full or partial ID (8+ chars)
        let matching_pattern = if id.len() >= 8 {
            store
                .patterns
                .iter()
                .find(|p| p.id.starts_with(id))
                .cloned()
        } else {
            None
        };

        let matching_exact = if id.len() >= 8 {
            store
                .exact_approvals
                .iter()
                .find(|a| a.id.starts_with(id))
                .cloned()
        } else {
            None
        };

        if matching_pattern.is_none() && matching_exact.is_none() {
            return Ok(format!("No pattern or approval found with ID: {}", id));
        }

        // Show what we're removing
        if let Some(ref pattern) = matching_pattern {
            self.output_status("Found pattern to remove:");
            self.output_status(format!("  ID: {}", &pattern.id[..8]));
            self.output_status(format!("  Tool: {}", pattern.tool_name));
            self.output_status(format!("  Pattern: {}", pattern.pattern));
            self.output_status(format!("  Match count: {}", pattern.match_count));
            self.output_status("");

            // Confirm if match count > 10
            if pattern.match_count > 10 {
                print!(
                    "This pattern has been used {} times. Remove? [y/N]: ",
                    pattern.match_count
                );
                io::stdout().flush()?;

                let mut confirm = String::new();
                io::stdin().read_line(&mut confirm)?;

                if !confirm.trim().eq_ignore_ascii_case("y") {
                    return Ok("Removal cancelled.".to_string());
                }
            }

            // Remove and save
            if self.tool_executor.lock().await.remove_pattern(&pattern.id) {
                self.tool_executor.lock().await.save_patterns()?;
                Ok(format!("Removed pattern: {}", &pattern.id[..8]))
            } else {
                Ok(format!("Failed to remove pattern: {}", id))
            }
        } else if let Some(ref approval) = matching_exact {
            self.output_status("Found exact approval to remove:");
            self.output_status(format!("  ID: {}", &approval.id[..8]));
            self.output_status(format!("  Tool: {}", approval.tool_name));
            self.output_status(format!("  Signature: {}", approval.signature));
            self.output_status(format!("  Match count: {}", approval.match_count));
            self.output_status("");

            // Confirm if match count > 10
            if approval.match_count > 10 {
                print!(
                    "This approval has been used {} times. Remove? [y/N]: ",
                    approval.match_count
                );
                io::stdout().flush()?;

                let mut confirm = String::new();
                io::stdin().read_line(&mut confirm)?;

                if !confirm.trim().eq_ignore_ascii_case("y") {
                    return Ok("Removal cancelled.".to_string());
                }
            }

            // Remove and save
            if self.tool_executor.lock().await.remove_pattern(&approval.id) {
                self.tool_executor.lock().await.save_patterns()?;
                Ok(format!("Removed approval: {}", &approval.id[..8]))
            } else {
                Ok(format!("Failed to remove approval: {}", id))
            }
        } else {
            Ok(format!("No pattern or approval found with ID: {}", id))
        }
    }

    /// Clear all patterns with confirmation
    pub async fn clear_patterns(&mut self) -> Result<String> {
        let executor = self.tool_executor.lock().await;
        let store = executor.persistent_store();
        let total = store.total_count();

        if total == 0 {
            return Ok("No patterns to clear.".to_string());
        }

        self.output_status(format!("This will remove {} pattern(s) and {} exact approval(s).", store.patterns.len(), store.exact_approvals.len()));

        if !Menu::confirm("Are you sure?", false)? {
            return Ok("Clear cancelled.".to_string());
        }

        // Clear and save
        self.tool_executor.lock().await.clear_persistent_patterns();
        self.tool_executor.lock().await.save_patterns()?;

        Ok(format!("Cleared {} pattern(s) and approval(s).", total))
    }

    /// Add a pattern interactively
    pub async fn add_pattern_interactive(&mut self) -> Result<String> {
        use crate::tools::patterns::PatternType;

        self.output_status("Add Confirmation Pattern");
        self.output_status("========================\n");

        // 1. Pattern type
        let pattern_type_options = vec![
            MenuOption::with_description(
                "Wildcard (*, **)",
                "Use * for wildcards, ** for recursive paths",
                PatternType::Wildcard,
            ),
            MenuOption::with_description(
                "Regex",
                "Use regular expression syntax",
                PatternType::Regex,
            ),
        ];

        let pattern_type = Menu::select(
            "Pattern type:",
            pattern_type_options,
            Some("[↑↓ or j/k to move, Enter to select, or type 1-2]"),
        )?;

        // 2. Tool name
        let tool_name = Menu::text_input(
            "Tool name:",
            None,
            Some("bash, read, grep, glob, web_fetch, save_and_exec"),
        )?;

        if tool_name.is_empty() {
            return Ok("Pattern creation cancelled (no tool name).".to_string());
        }

        // 3. Pattern string (with help)
        self.output_status("\nPattern syntax:");
        match pattern_type {
            PatternType::Wildcard => {
                self.output_status("  * = match anything (single component)");
                self.output_status("  ** = match anything recursively (paths)");
                self.output_status("Examples:");
                self.output_status("  cargo * in /project");
                self.output_status("  reading /project/**");
                self.output_status("  cargo * in *");
            }
            PatternType::Regex => {
                self.output_status("  Standard regex syntax");
                self.output_status("Examples:");
                self.output_status("  ^cargo (test|build)$");
                self.output_status(r"  reading /project/src/.*\.rs$");
            }
            PatternType::Structured => {
                self.output_status("  Match command, args, and directory separately");
                self.output_status("  Use * to match anything, or specific values");
                self.output_status("Examples:");
                self.output_status("  cmd:cargo args:test dir:/project");
                self.output_status("  cmd:git args:* dir:/home/*/projects");
                self.output_status("  cmd:* args:--release dir:*");
            }
        }

        let pattern_str = Menu::text_input("Pattern:", None, Some("Enter the pattern string"))?;

        if pattern_str.is_empty() {
            return Ok("Pattern creation cancelled (no pattern).".to_string());
        }

        // 4. Description
        let description = Menu::text_input(
            "Description:",
            None,
            Some("Brief description of what this pattern allows"),
        )?;

        // 5. Create pattern and validate
        let pattern = ToolPattern::new_with_type(
            pattern_str.clone(),
            tool_name.clone(),
            description,
            pattern_type,
        );

        if let Err(e) = pattern.validate() {
            return Ok(format!("Invalid pattern: {}", e));
        }

        // 6. Optional: Test pattern
        self.output_status("\nPattern created:");
        self.output_status(format!("  Tool: {}", pattern.tool_name));
        self.output_status(format!("  Pattern: {}", pattern.pattern));
        self.output_status(format!("  Type: {:?}", pattern.pattern_type));

        if Menu::confirm("\nTest pattern?", false)? {
            let test_str = Menu::text_input(
                "Enter test string:",
                None,
                Some("String to test against the pattern"),
            )?;

            let test_sig = ToolSignature {
                tool_name: pattern.tool_name.clone(),
                context_key: test_str,
                command: None,
                args: None,
                directory: None,
            };

            if pattern.matches(&test_sig) {
                self.output_status("✓ Pattern matches!");
            } else {
                self.output_status("✗ Pattern does not match.");
            }
            self.output_status("");
        }

        // 7. Confirm and save
        print!("Save pattern? [Y/n]: ");
        io::stdout().flush()?;

        let mut save_choice = String::new();
        io::stdin().read_line(&mut save_choice)?;

        if save_choice.trim().eq_ignore_ascii_case("n") {
            return Ok("Pattern creation cancelled.".to_string());
        }

        // Add to executor and save
        self.tool_executor
            .lock()
            .await
            .approve_pattern_persistent(pattern.clone());
        self.tool_executor.lock().await.save_patterns()?;

        Ok(format!(
            "Pattern saved: {} ({})",
            &pattern.id[..8],
            pattern.pattern
        ))
    }

    /// Get mode-specific prompt string
    fn get_prompt(&self) -> String {
        match &self.mode {
            ReplMode::Normal => "> ".to_string(),
            ReplMode::Planning { .. } => format!("{} ", "plan>".blue()),
            ReplMode::Executing { .. } => format!("{} ", "exec>".green()),
        }
    }

    /// Print training status below the prompt (only in interactive mode)
    async fn print_status_line(&self) {
        if !self.is_interactive {
            return;
        }

        let router_stats = self.router.stats(); // CHANGED: use router.stats()
        let validator_stats = self.threshold_validator.stats();

        // Calculate percentages
        let local_pct = if router_stats.total_queries == 0 {
            0.0
        } else {
            (router_stats.total_local_attempts as f64 / router_stats.total_queries as f64) * 100.0
        };

        let success_pct = if router_stats.total_local_attempts == 0 {
            0.0
        } else {
            (router_stats.total_successes as f64 / router_stats.total_local_attempts as f64) * 100.0
        };

        // Get training metrics
        let quality_avg = self.training_trends.avg_quality();
        let similarity_avg = self.training_trends.avg_similarity();

        // Build mode indicator
        let mode_indicator = match &self.mode {
            ReplMode::Normal => String::new(),
            ReplMode::Planning { .. } => {
                format!(" {}", "[PLANNING MODE - Inspection Only]".blue().bold())
            }
            ReplMode::Executing { .. } => format!(" {}", "[EXECUTING PLAN]".green().bold()),
        };

        // Build single-line status string with training effectiveness and conversation context
        let turn_count = self.conversation.read().await.turn_count();
        let context_indicator = if turn_count > 0 {
            format!(" | Context: {} turns", turn_count)
        } else {
            String::new()
        };

        let status = if self.training_trends.measurement_count() > 0 {
            format!(
                "{}Training: {} queries | Local: {:.0}% | Success: {:.0}% | Quality: {:.2} | Similarity: {:.2} | Confidence: {:.2}{}",
                mode_indicator,
                router_stats.total_queries,
                local_pct,
                success_pct,
                quality_avg,
                similarity_avg,
                router_stats.confidence_threshold,
                context_indicator
            )
        } else {
            // Fallback if no training data yet
            format!(
                "{}Training: {} queries | Local: {:.0}% | Success: {:.0}% | Confidence: {:.2} | Approval: {:.0}%{}",
                mode_indicator,
                router_stats.total_queries,
                local_pct,
                success_pct,
                router_stats.confidence_threshold,
                validator_stats.approval_rate * 100.0,
                context_indicator
            )
        };

        // Truncate to terminal width if needed
        let width = terminal_width();
        let truncated = if status.len() > width {
            format!("{}...", &status[..width.saturating_sub(3)])
        } else {
            status
        };

        // Print in gray, all on one line
        self.output_status(format!("{}", truncated.dark_grey()));
    }

    pub async fn process_query(&mut self, query: &str) -> Result<String> {
        let start_time = Instant::now();

        // Phase 2: Inject persona system prompt + memory context if first message in conversation
        {
            let conv = self.conversation.read().await;
            if conv.get_messages().is_empty() {
                drop(conv);

                let base_system = {
                    let persona = self.active_persona.read().await;
                    persona.to_system_message()
                };

                // Augment system prompt with semantically relevant memories from previous sessions
                let system_prompt = if let Some(ref memory) = self.memory_system {
                    match memory.query(query, Some(5)).await {
                        Ok(memories) if !memories.is_empty() => {
                            let memory_block = memories
                                .iter()
                                .map(|m| format!("• {}", m))
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!(
                                "{}\n\n## Memories from previous sessions\n{}",
                                base_system, memory_block
                            )
                        }
                        _ => base_system,
                    }
                } else {
                    base_system
                };

                // Add as a Message with system role
                use crate::claude::{Message, ContentBlock};
                let system_msg = Message {
                    role: "system".to_string(),
                    content: vec![ContentBlock::Text { text: system_prompt }],
                };
                self.conversation.write().await.add_message(system_msg);
            }
        }

        // Add user message to conversation history
        self.conversation.write().await.add_user_message(query.to_string());

        if self.is_interactive {
            print!("{}", "Analyzing...".dark_grey());
            io::stdout().flush()?;
        }

        // DAEMON MODE: If daemon client is available, use it
        if let Some(daemon_client) = &self.daemon_client {
            if self.is_interactive {
                io::stdout()
                    .execute(cursor::MoveToColumn(0))?
                    .execute(Clear(ClearType::CurrentLine))?;
                self.output_status("→ Using daemon for query");
            }

            match daemon_client.query_text(query).await {
                Ok(response) => {
                    // Add assistant response to conversation
                    self.conversation.write().await.add_assistant_message(response.clone());

                    if self.is_interactive {
                        let elapsed = start_time.elapsed();
                        self.output_status(format!("✓ Query completed in {:.2}s (via daemon)", elapsed.as_secs_f64()));
                    }

                    return Ok(response);
                }
                Err(e) => {
                    tracing::warn!("Daemon query failed: {}, falling back to teacher", e);
                    if self.is_interactive {
                        self.output_status(format!("⚠️  Daemon failed: {}", e));
                        self.output_status("→ Falling back to teacher API");
                    }
                    // Fall through to normal routing below
                }
            }
        }

        // FALLBACK: Normal routing (local model or teacher API)
        // Make routing decision (uses threshold router internally)
        // Check if local generator is ready before routing (progressive bootstrap support)
        let generator_ready = matches!(
            *self.bootstrap_loader.state().read().await,
            GeneratorState::Ready { .. }
        );
        let decision = self.router.route_with_generator_check(query, generator_ready);

        if self.is_interactive {
            io::stdout()
                .execute(cursor::MoveToColumn(0))?
                .execute(Clear(ClearType::CurrentLine))?;
        }

        // Track local and final responses for comparison
        let mut local_response: Option<String> = None;
        let claude_response: String;
        let routing_decision_str: String;
        let mut pattern_id: Option<String> = None;
        let mut routing_confidence: Option<f64> = None;
        let mut forward_reason: Option<String> = None;

        match decision {
            RouteDecision::Local {
                pattern_id: local_pattern_id,
                confidence,
            } => {
                if self.is_interactive {
                    self.output_status("✓ Crisis check: PASS");
                    self.output_status(format!("✓ Threshold check: PASS (confidence: {:.2})", confidence));
                    self.output_status("→ Routing: LOCAL GENERATION");
                }

                // Double-check generator state before attempting local generation
                // (Belt-and-suspenders approach - router should have already checked)
                let is_model_ready = {
                    let state = self.bootstrap_loader.state().read().await;
                    let ready = matches!(*state, GeneratorState::Ready { .. });
                    if !ready && self.is_interactive {
                        self.output_status(format!("⚠ Model not ready: {}", state.status_message()));
                        self.output_status("→ Forwarding to Claude");
                    }
                    ready
                }; // Drop the state guard here

                if !is_model_ready {
                    // Model not ready, forward to Claude
                    let request =
                        MessageRequest::with_context(self.conversation.read().await.get_messages())
                            .with_tools(self.tool_definitions.clone());

                    // Try streaming first, fallback to buffered if tools detected
                    let use_streaming = self.streaming_enabled && self.is_interactive;

                    if use_streaming {
                        let rx = self.call_teacher_stream(&request).await?;
                        match self.display_streaming_response(rx).await {
                            Ok(text) => {
                                claude_response = text;
                            }
                            Err(e) if e.to_string().contains("TOOLS_DETECTED") => {
                                if self.is_interactive {
                                    self.output_status("\n🔧 Tools needed - switching to buffered mode...");
                                }
                                let response = self.call_teacher(&request).await?;
                                let elapsed = start_time.elapsed().as_millis();
                                if self.is_interactive {
                                    self.output_status(format!("✓ Received response ({}ms)", elapsed));
                                }
                                claude_response = self.execute_tool_loop(response).await?;
                            }
                            Err(e) => return Err(e),
                        }
                    } else {
                        let response = self.call_teacher(&request).await?;
                        let elapsed = start_time.elapsed().as_millis();
                        if self.is_interactive {
                            self.output_status(format!("✓ Received response ({}ms)", elapsed));
                        }
                        if response.has_tool_uses() {
                            claude_response = self.execute_tool_loop(response).await?;
                        } else {
                            claude_response = response.text();
                        }
                    }

                    routing_decision_str = "forward".to_string();
                    forward_reason = Some("model_not_ready".to_string());
                } else {
                    // Model is ready, proceed with local generation
                    // Try local generation
                    let mut gen = self.local_generator.write().await;
                    match gen.try_generate_from_pattern(query) {
                    Ok(Some(response_text)) => {
                        // Successfully generated locally
                        if self.is_interactive {
                            self.output_status(format!("✓ Generated locally (confidence: {:.2})", confidence));
                        }
                        local_response = Some(response_text.clone());
                        claude_response = response_text;
                        routing_decision_str = "local".to_string();
                        pattern_id = Some(local_pattern_id);
                        routing_confidence = Some(confidence);
                    }
                    Ok(None) | Err(_) => {
                        // Local generation insufficient or failed - forward to Claude
                        drop(gen); // Drop the read lock before forwarding to Claude

                        if self.is_interactive {
                            self.output_status("⚠️  Local generation insufficient confidence");
                            self.output_status("→ Forwarding to Claude");
                        }

                        // Forward to Claude
                        let request =
                            MessageRequest::with_context(self.conversation.read().await.get_messages())
                                .with_tools(self.tool_definitions.clone());

                        // Try streaming first, fallback to buffered if tools detected
                        let use_streaming = self.streaming_enabled && self.is_interactive;

                        if use_streaming {
                            let rx = self.call_teacher_stream(&request).await?;
                            match self.display_streaming_response(rx).await {
                                Ok(text) => {
                                    claude_response = text;
                                }
                                Err(e) if e.to_string().contains("TOOLS_DETECTED") => {
                                    if self.is_interactive {
                                        self.output_status("
🔧 Tools needed - switching to buffered mode...");
                                    }
                                    let response =
                                        self.call_teacher(&request).await?;
                                    let elapsed = start_time.elapsed().as_millis();
                                    if self.is_interactive {
                                        self.output_status(format!("✓ Received response ({}ms)", elapsed));
                                    }
                                    claude_response = self.execute_tool_loop(response).await?;
                                }
                                Err(e) => return Err(e),
                            }
                        } else {
                            let response = self.call_teacher(&request).await?;
                            let elapsed = start_time.elapsed().as_millis();
                            if self.is_interactive {
                                self.output_status(format!("✓ Received response ({}ms)", elapsed));
                            }
                            if response.has_tool_uses() {
                                claude_response = self.execute_tool_loop(response).await?;
                            } else {
                                claude_response = response.text();
                            }
                        }

                        routing_decision_str = "local_attempted".to_string();
                        pattern_id = Some(local_pattern_id);
                        routing_confidence = Some(confidence);
                        forward_reason = Some("insufficient_confidence".to_string());
                    }
                    } // Close the else block for generator readiness check
                }
            }
            RouteDecision::Forward { reason } => {
                if self.is_interactive {
                    match reason {
                        ForwardReason::ModelNotReady => {
                            let status_msg = {
                                let state = self.bootstrap_loader.state().read().await;
                                state.status_message()
                            }; // Drop the state guard here
                            self.output_status(format!("ℹ️  Model status: {}", status_msg));
                            self.output_status("→ Routing: FORWARDING TO TEACHER");
                        }
                        _ => {
                            self.output_status("✗ Threshold check: FAIL (confidence too low)");
                            self.output_status("→ Routing: FORWARDING TO TEACHER");
                        }
                    }
                }

                // Use full conversation context with tool definitions
                let request = MessageRequest::with_context(self.conversation.read().await.get_messages())
                    .with_tools(self.tool_definitions.clone());

                // Try streaming first, fallback to buffered if tools detected
                let use_streaming = self.streaming_enabled && self.is_interactive;

                if use_streaming {
                    // Streaming path - will abort if tools detected
                    let rx = self.call_teacher_stream(&request).await?;
                    match self.display_streaming_response(rx).await {
                        Ok(text) => {
                            // Streaming succeeded (no tools)
                            claude_response = text;
                        }
                        Err(e) if e.to_string().contains("TOOLS_DETECTED") => {
                            // Tools detected in stream - fallback to buffered mode
                            if self.is_interactive {
                                self.output_status("\n🔧 Tools needed - switching to buffered mode...");
                            }
                            let response = self.call_teacher(&request).await?;

                            let elapsed = start_time.elapsed().as_millis();
                            if self.is_interactive {
                                self.output_status(format!("✓ Received response ({}ms)", elapsed));
                            }

                            // Execute tool loop
                            claude_response = self.execute_tool_loop(response).await?;
                        }
                        Err(e) => {
                            // Real error - propagate
                            return Err(e);
                        }
                    }
                } else {
                    // Non-streaming path (supports tool use detection)
                    let response = self.call_teacher(&request).await?;

                    let elapsed = start_time.elapsed().as_millis();
                    if self.is_interactive {
                        self.output_status(format!("✓ Received response ({}ms)", elapsed));
                    }

                    // Check for tool uses and execute them
                    if response.has_tool_uses() {
                        claude_response = self.execute_tool_loop(response).await?;
                    } else {
                        claude_response = response.text();
                    }
                }
                routing_decision_str = "forward".to_string();
                forward_reason = Some(reason.as_str().to_string());
            }
        };

        // Calculate quality and similarity
        let quality_score = self
            .threshold_validator
            .quality_score(query, &claude_response);

        let (similarity_score, divergence) = if let Some(ref local_resp) = local_response {
            use crate::metrics::semantic_similarity;
            let sim = semantic_similarity(local_resp, &claude_response)?;
            (Some(sim), Some(1.0 - sim))
        } else {
            (None, None)
        };

        // Online learning: Update threshold models
        if self.is_interactive {
            self.output_status("");
            print!("{}", "Learning... ".dark_grey());
            io::stdout().flush()?;
        }

        // Learn from this interaction
        match routing_decision_str.as_str() {
            "local" => {
                // We successfully generated locally
                let was_successful = quality_score >= 0.7;
                self.router.learn_local_attempt(query, was_successful);
            }
            "local_attempted" => {
                // We tried local but fell back to Claude (always counts as failure)
                self.router.learn_local_attempt(query, false);
            }
            "forward" => {
                // We forwarded directly to Claude (no local attempt)
                self.router.learn_forwarded(query);
            }
            _ => {
                tracing::warn!("Unknown routing decision: {}", routing_decision_str);
            }
        }

        self.threshold_validator
            .learn(query, &claude_response, quality_score >= 0.7);

        // Learn from Claude response (for local generation and neural training)
        {
            let mut gen = self.local_generator.write().await;
            gen.learn_from_claude(
                query,
                &claude_response,
                quality_score,
                None, // TODO: Add training via BootstrapLoader's Qwen generator
            );
        }

        // Update training trends
        self.training_trends
            .add_measurement(quality_score, similarity_score);

        // Checkpoint every 10 queries
        let router_stats = self.router.stats(); // CHANGED: use router.stats()
        if router_stats.total_queries % 10 == 0 && router_stats.total_queries > 0 {
            let _ = self.save_models().await; // Ignore errors during checkpoint
        }

        if self.is_interactive {
            self.output_status("✓");
        }

        // Log metric with comparison data
        let query_hash = MetricsLogger::hash_query(query);
        let response_time_ms = start_time.elapsed().as_millis() as u64;

        let comparison = ResponseComparison {
            local_response: local_response.clone(),
            claude_response: claude_response.clone(),
            quality_score,
            similarity_score,
            divergence,
        };

        let router_confidence = Some(router_stats.confidence_threshold);
        let validator_confidence = Some(quality_score);

        // Phase 1: Log conversation BEFORE creating metric (routing_decision_str is moved)
        let model_name = if routing_decision_str == "local" {
            "local".to_string()
        } else {
            "teacher".to_string()
        };
        let tools_used = vec![]; // TODO: Track tools used during execution
        if let Err(e) = self.conversation_logger
            .lock()
            .await
            .log_interaction(query, &claude_response, &model_name, &tools_used)
            .await
        {
            tracing::warn!("Failed to log conversation: {}", e);
        }

        let metric = RequestMetric::new(
            query_hash,
            routing_decision_str,
            pattern_id,
            routing_confidence,
            forward_reason,
            response_time_ms,
            comparison,
            router_confidence,
            validator_confidence,
        );

        self.metrics_logger.log(&metric)?;

        // Auto-save plan if in planning mode
        if let ReplMode::Planning { plan_path, .. } = &self.mode {
            // Detect if response contains a plan (improved detection)
            let response_lower = claude_response.to_lowercase();
            let markers = ["# plan", "## plan", "## analysis", "## proposed"];
            let has_marker = markers.iter().any(|m| response_lower.contains(m));
            let is_long = claude_response.len() > 500;

            if has_marker || is_long {
                if let Err(e) = std::fs::write(plan_path, &claude_response) {
                    self.output_error(format!("Warning: Failed to save plan: {}", e));
                } else if self.is_interactive {
                    self.output_status(format!("\n✓ Plan saved to: {}", plan_path.display()));
                    self.output_status("Type /show-plan to review, /approve to execute, /reject to cancel.");
                }
            }
        }

        // Add assistant response to conversation history
        self.conversation.write().await.add_assistant_message(claude_response.clone());

        // Phase 4: Store conversation in memory automatically
        if let Some(ref memory) = self.memory_system {
            if let Err(e) = memory.insert_conversation("user", query, Some(&model_name), None).await {
                tracing::warn!("Failed to store user message in memory: {}", e);
            }
            if let Err(e) = memory.insert_conversation("assistant", &claude_response, Some(&model_name), None).await {
                tracing::warn!("Failed to store assistant message in memory: {}", e);
            }
        }

        // Store last query/response for feedback commands
        self.last_query = Some(query.to_string());
        self.last_response = Some(claude_response.clone());
        self.last_was_sampled = false; // TODO: Set based on sampling decision

        Ok(claude_response)
    }

    /// Handle feedback commands - add weighted training example
    async fn handle_feedback(&mut self, weight: f64, note: Option<String>) -> Result<()> {
        // Check if we have a last query/response to provide feedback on
        let query = match &self.last_query {
            Some(q) => q.clone(),
            None => {
                self.output_status("⚠️  No previous query to provide feedback on.");
                self.output_status("    Use feedback commands after receiving a response.");
                return Ok(());
            }
        };

        let response = match &self.last_response {
            Some(r) => r.clone(),
            None => {
                self.output_status("⚠️  No previous response to provide feedback on.");
                return Ok(());
            }
        };

        // Create feedback message
        let feedback = note
            .as_ref()
            .map(|s| s.clone())
            .unwrap_or_else(|| match weight as i32 {
                10 => "Critical issue that needs correction".to_string(),
                3 => "Could be improved".to_string(),
                1 => "Good example to remember".to_string(),
                _ => "User feedback".to_string(),
            });

        // Create weighted example
        let example = match weight as i32 {
            10 => WeightedExample::critical(query.clone(), response.clone(), feedback.clone()),
            3 => WeightedExample::improvement(query.clone(), response.clone(), feedback.clone()),
            1 => WeightedExample::normal(query.clone(), response.clone(), feedback.clone()),
            _ => WeightedExample::with_weight(
                query.clone(),
                response.clone(),
                feedback.clone(),
                weight,
            ),
        };

        // Add to training coordinator
        let should_train = self
            .training_coordinator
            .add_example(example)
            .context("Failed to add training example")?;

        // Display confirmation
        let weight_emoji = match weight as i32 {
            10 => "🔴",
            3 => "🟡",
            1 => "🟢",
            _ => "⚪",
        };

        self.output_status(format!("{} Feedback recorded (weight: {}x)", weight_emoji, weight));
        if let Some(note_text) = note {
            self.output_status(format!("   Note: {}", note_text));
        }

        // Get buffer stats
        let buffer = self.training_coordinator.buffer()?;
        let example_count = buffer.examples().len();
        let total_weight = buffer.total_weight();
        drop(buffer); // Release lock

        self.output_status(format!(
            "   Training buffer: {} examples ({:.1} weighted)",
            example_count, total_weight
        ));

        // Trigger training if threshold reached
        if should_train {
            self.output_status("\n🔄 Training threshold reached, starting background training...");
            self.output_status("   (Training runs in background, you can continue querying)");

            // Spawn background training task
            let coordinator = Arc::clone(&self.training_coordinator);
            let models_dir = self.models_dir.clone();

            tokio::spawn(async move {
                match Self::run_background_training(coordinator, models_dir).await {
                    Ok(stats) => {
                        output_status!("\n✓ {} examples queued for offline training", stats.examples_trained);
                        output_status!("   Queue: {}", stats.queue_path);
                        output_status!("   To train: python3 scripts/train_lora.py {} \\", stats.queue_path);
                        output_status!("             ~/.finch/adapters/latest.safetensors");
                        output_status!("   (See GitHub Issue #1 for adapter re-loading into ONNX)");
                    }
                    Err(e) => {
                        output_error!("\n⚠️  Training queue export failed: {}", e);
                    }
                }
            });

            self.output_status("   Training started in background...");
        }

        Ok(())
    }

    /// Handle /local command - query local model directly (bypass routing)
    async fn handle_local_query(&mut self, query: &str) -> Result<()> {
        // Show status
        self.output_status("🔧 Local Model Query (bypassing routing)");

        // Check if daemon client exists
        if let Some(daemon_client) = &self.daemon_client {
            // Daemon mode: use HTTP
            if self.is_interactive {
                self.output_user(query);
            }

            // Start response
            let start_time = Instant::now();

            // Query daemon with local_only flag
            match daemon_client.query_local_only(query).await {
                Ok(response) => {
                    let elapsed = start_time.elapsed();

                    // Output the response
                    if self.is_interactive {
                        self.output_response(&response);
                    } else {
                        println!("{}", response);
                    }

                    // Show timing
                    if self.is_interactive {
                        self.output_status(format!("✓ Local model ({:.2}s)", elapsed.as_secs_f64()));
                    }
                }
                Err(e) => {
                    // Show error
                    let error_msg = format!("Error: {}", e);
                    if self.is_interactive {
                        self.output_error(&error_msg);
                        self.output_status("⚠️  Local model query failed");
                    } else {
                        eprintln!("{}", error_msg);
                    }
                }
            }
        } else {
            // No daemon mode - show error
            let error_msg =
                "Error: /local requires daemon mode. The daemon provides direct model access.";
            if self.is_interactive {
                self.output_status("⚠️  Daemon not available");
                self.output_status("    Start the daemon: finch daemon --bind 127.0.0.1:11435");
            } else {
                eprintln!("{}", error_msg);
            }
        }

        Ok(())
    }

    /// Phase 2: Handle /persona list command
    async fn handle_persona_list(&self) -> Result<()> {
        self.output_status("📋 Available Personas:\n");

        let builtin_personas = vec![
            ("default", "Balanced helpful assistant"),
            ("expert-coder", "Code review focus, best practices"),
            ("teacher", "Patient, educational, step-by-step"),
            ("analyst", "Data-focused, structured, citations"),
            ("creative", "Brainstorming, storytelling, creative solutions"),
            ("researcher", "Deep research, citations, fact-checking"),
        ];

        let current_persona = self.active_persona.read().await;
        let current_name = current_persona.name().to_lowercase();

        for (name, desc) in builtin_personas {
            let marker = if name == current_name { "→ " } else { "  " };
            self.output_status(format!("{}• {} - {}", marker, name, desc));
        }

        self.output_status("\nUse '/persona select <name>' to switch personas");
        Ok(())
    }

    /// Phase 2: Handle /persona select <name> command
    async fn handle_persona_select(&mut self, name: &str) -> Result<()> {
        // Try to load the persona
        match crate::config::Persona::load_builtin(name) {
            Ok(persona) => {
                let old_name = self.active_persona.read().await.name().to_string();
                *self.active_persona.write().await = persona;

                // Clear conversation to apply new persona from scratch
                self.conversation.write().await.clear();

                self.output_status(format!("✓ Switched persona: {} → {}", old_name, name));
                self.output_status("  (Conversation cleared to apply new persona)");
                Ok(())
            }
            Err(e) => {
                self.output_error(format!("Failed to load persona '{}': {}", name, e));
                self.output_status("Use '/persona list' to see available personas");
                Ok(())
            }
        }
    }

    /// Phase 2: Handle /persona show command
    async fn handle_persona_show(&self) -> Result<()> {
        let persona = self.active_persona.read().await;

        self.output_status(format!("📖 Current Persona: {}\n", persona.name()));
        self.output_status(format!("Tone: {}", persona.tone()));
        self.output_status(format!("Verbosity: {}", persona.verbosity()));
        self.output_status(format!("Focus: {}\n", persona.focus()));
        self.output_status("System Prompt:");
        self.output_status("─".repeat(60));
        self.output_status(&persona.behavior.system_prompt);
        self.output_status("─".repeat(60));

        if !persona.behavior.examples.is_empty() {
            self.output_status(format!("\nExamples ({}):", persona.behavior.examples.len()));
            for (i, example) in persona.behavior.examples.iter().take(2).enumerate() {
                self.output_status(format!("\n{}. User: {}", i + 1, example.user));
                self.output_status(format!("   Assistant: {}", example.assistant));
            }
        }

        Ok(())
    }

    /// Handle /model list command
    async fn handle_model_list(&self) -> Result<()> {
        self.output_status("📋 Available Providers:\n");

        for (idx, provider) in self.available_providers.iter().enumerate() {
            let marker = if idx == self.active_provider_index { "→ " } else { "  " };
            let tag = if provider.is_local() { "[local]" } else { "[cloud]" };
            let name = provider.display_name();
            let ptype = provider.provider_type();

            let detail = if provider.is_local() {
                if let crate::config::ProviderEntry::Local { model_family, model_size, execution_target, .. } = provider {
                    format!("{:?} {:?} on {:?}", model_family, model_size, execution_target)
                } else {
                    String::new()
                }
            } else {
                provider.model().unwrap_or("(default model)").to_string()
            };

            self.output_status(format!(
                "{}{}. {} {} ({}) — {}",
                marker,
                idx + 1,
                tag,
                name,
                ptype,
                detail,
            ));
        }

        self.output_status("\nUse '/model <name>' or '/model <number>' to switch");
        self.output_status("Memory will be preserved across switches");
        Ok(())
    }

    /// Handle /model <name> command
    async fn handle_model_switch(&mut self, name: &str) -> Result<()> {
        // Try to parse as number first (1-indexed)
        let target_index = if let Ok(num) = name.parse::<usize>() {
            if num == 0 || num > self.available_providers.len() {
                self.output_error(format!(
                    "Invalid model number: {}. Use 1-{}",
                    num,
                    self.available_providers.len()
                ));
                return Ok(());
            }
            num - 1
        } else {
            // Find by provider type or display name
            match self.available_providers.iter().position(|p| {
                p.provider_type().eq_ignore_ascii_case(name)
                    || p.display_name().eq_ignore_ascii_case(name)
            }) {
                Some(idx) => idx,
                None => {
                    self.output_error(format!("Provider '{}' not found", name));
                    self.output_status("Use '/model list' to see available providers");
                    return Ok(());
                }
            }
        };

        if target_index == self.active_provider_index {
            self.output_status("Already using this provider");
            return Ok(());
        }

        let new_entry = self.available_providers[target_index].clone();
        let old_name = self.available_providers[self.active_provider_index]
            .display_name()
            .to_string();

        if new_entry.is_local() {
            // Local provider — just update the index; the event loop uses the
            // local generator for queries, not the teacher session.
            self.active_provider_index = target_index;
            // Keep active_teacher_index pointing at the first cloud provider
            // for when we fall back to the cloud path.
            self.output_status(format!(
                "✓ Switched provider: {} → {} (local inference)",
                old_name,
                new_entry.display_name()
            ));
        } else {
            // Cloud provider — create a new teacher session.
            let llm_provider = match crate::providers::create_provider_from_entry(&new_entry) {
                Ok(p) => p,
                Err(e) => {
                    self.output_error(format!("Failed to create provider: {}", e));
                    return Ok(());
                }
            };

            let teacher_config = TeacherContextConfig {
                max_context_turns: 15,
                tool_result_retention_turns: 5,
                prompt_caching_enabled: true,
            };

            *self.teacher_session.write().await =
                TeacherSession::with_config(llm_provider, teacher_config);

            // Keep available_teachers index in sync
            if let Some(new_teacher_idx) = self
                .available_teachers
                .iter()
                .position(|t| t.provider.eq_ignore_ascii_case(new_entry.provider_type()))
            {
                self.active_teacher_index = new_teacher_idx;
            }
            self.active_provider_index = target_index;

            let new_name = new_entry.display_name().to_string();
            let memory_info = if let Some(ref memory) = self.memory_system {
                let stats = memory.stats().await?;
                format!(" (💾 {} nodes in memory)", stats.tree_node_count)
            } else {
                String::new()
            };
            self.output_status(format!(
                "✓ Switched provider: {} → {}{}",
                old_name, new_name, memory_info
            ));
        }

        Ok(())
    }

    /// Handle /model show command
    async fn handle_model_show(&self) -> Result<()> {
        let active = &self.available_providers[self.active_provider_index];
        self.output_status(format!("📖 Current Provider: {}", active.display_name()));
        self.output_status(format!("Type: {}", active.provider_type()));

        if active.is_local() {
            if let crate::config::ProviderEntry::Local {
                model_family,
                model_size,
                inference_provider,
                execution_target,
                ..
            } = active
            {
                self.output_status(format!("Family: {:?}", model_family));
                self.output_status(format!("Size: {:?}", model_size));
                self.output_status(format!("Backend: {:?}", inference_provider));
                self.output_status(format!("Device: {:?}", execution_target));
            }
        } else {
            let model = active.model().unwrap_or("(provider default)");
            self.output_status(format!("Model: {}", model));
        }

        // Get conversation stats
        let conv = self.conversation.read().await;
        let message_count = conv.message_count();

        self.output_status(format!("\nConversation: {} messages", message_count));

        // Get memory stats if available
        if let Some(ref memory) = self.memory_system {
            let stats = memory.stats().await?;
            self.output_status(format!("Memory: {} nodes", stats.tree_node_count));
        }

        Ok(())
    }

    /// Phase 3: Handle /discover command (mDNS service discovery)
    async fn handle_discover(&self) -> Result<()> {
        use crate::service::ServiceDiscoveryClient;
        use std::time::Duration;

        self.output_status("🔍 Discovering Finch instances on local network...\n");

        // Create discovery client
        let client = match ServiceDiscoveryClient::new() {
            Ok(c) => c,
            Err(e) => {
                self.output_error(format!("Failed to create discovery client: {}", e));
                return Ok(());
            }
        };

        // Discover services (5 second timeout)
        let services = match client.discover(Duration::from_secs(5)) {
            Ok(s) => s,
            Err(e) => {
                self.output_error(format!("Failed to discover services: {}", e));
                return Ok(());
            }
        };

        if services.is_empty() {
            self.output_status("No Finch instances found on the local network.");
            self.output_status("\nTo start a daemon in advertising mode:");
            self.output_status("  1. Edit ~/.finch/config.toml");
            self.output_status("  2. Set [server] advertise = true");
            self.output_status("  3. Run: finch daemon");
        } else {
            self.output_status(format!("Found {} Finch instance(s):\n", services.len()));

            for (i, service) in services.iter().enumerate() {
                self.output_status(format!("{}. {}", i + 1, service.name));
                self.output_status(format!("   Host: {}:{}", service.host, service.port));
                self.output_status(format!("   Model: {}", service.model));
                if !service.description.is_empty() {
                    self.output_status(format!("   Description: {}", service.description));
                }
                if !service.capabilities.is_empty() {
                    self.output_status(format!("   Capabilities: {}", service.capabilities.join(", ")));
                }
                self.output_status("");
            }

            self.output_status("To connect to a remote daemon:");
            self.output_status("  1. Edit ~/.finch/config.toml");
            self.output_status("  2. Set [client] daemon_address = \"HOST:PORT\"");
            self.output_status("  3. Restart Finch");
        }

        Ok(())
    }

    /// Phase 4: Handle /memory command (memory statistics)
    async fn handle_memory_stats(&self) -> Result<()> {
        if let Some(memory) = &self.memory_system {
            let stats = memory.stats().await?;

            self.output_status("📚 Memory System Statistics:\n");
            self.output_status(format!("Conversations stored: {}", stats.conversation_count));
            self.output_status(format!("MemTree nodes: {}", stats.tree_node_count));
            self.output_status("");

            if stats.conversation_count > 0 {
                // Show recent conversations
                let recent = memory.get_recent_conversations(5).await?;
                if !recent.is_empty() {
                    self.output_status("Recent conversations:");
                    for (i, (role, content)) in recent.iter().enumerate().take(5) {
                        let preview = if content.len() > 60 {
                            format!("{}...", &content[..60])
                        } else {
                            content.clone()
                        };
                        self.output_status(format!("{}. {}: {}", i + 1, role, preview));
                    }
                }
            }

            self.output_status("\nMemory queries are automatically included in your prompts");
            self.output_status("for better context across sessions.");
        } else {
            self.output_status("Memory system is disabled.");
            self.output_status("\nTo enable memory:");
            self.output_status("  1. Edit ~/.finch/config.toml");
            self.output_status("  2. Add [memory] section with enabled = true");
            self.output_status("  3. Restart Finch");
        }

        Ok(())
    }

    /// Handle /compact command - clear history but keep a summary
    async fn handle_compact_command(&mut self, instruction: Option<String>) -> Result<()> {
        // Get current conversation history
        let conversation = self.conversation.read().await;
        let history = conversation.get_messages();

        if history.is_empty() {
            self.output_status("No conversation history to compact.");
            return Ok(());
        }

        // Build a prompt to summarize the conversation
        let summary_prompt = if let Some(inst) = instruction {
            format!(
                "Please summarize the following conversation, focusing on: {}\n\n\
                Conversation history:\n{}",
                inst,
                history
                    .iter()
                    .map(|msg| format!("{}: {}", msg.role, msg.text_content()))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
        } else {
            format!(
                "Please provide a concise summary of the following conversation, \
                highlighting key topics, decisions, and any unresolved questions:\n\n\
                Conversation history:\n{}",
                history
                    .iter()
                    .map(|msg| format!("{}: {}", msg.role, msg.text_content()))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
        };

        drop(conversation); // Release lock before async operations

        self.output_status("📝 Generating conversation summary...");

        // Get summary from teacher
        use crate::claude::types::Message;
        use crate::providers::ProviderRequest;

        let request = ProviderRequest::new(vec![Message::user(summary_prompt)]);
        let summary_result = self
            .teacher_session
            .write()
            .await
            .send_message(&request)
            .await;

        match summary_result {
            Ok(response) => {
                // Extract text from response
                let summary = response.text();

                // Clear conversation
                self.conversation.write().await.clear();

                // Add summary as a system message to maintain context
                let system_msg = Message::with_content(
                    "system",
                    vec![crate::claude::types::ContentBlock::text(
                        format!("Previous conversation summary: {}", summary.trim())
                    )]
                );
                self.conversation.write().await.add_message(system_msg);

                self.output_status("✓ Conversation compacted. Context preserved in summary.");
                if self.is_interactive {
                    self.output_status("");
                    self.print_status_line().await;
                }
            }
            Err(e) => {
                self.output_status(format!("⚠️  Failed to generate summary: {}", e));
                self.output_status("   Conversation history not modified.");
            }
        }

        Ok(())
    }

    /// Export training examples to the offline queue (JSONL) for later Python-based LoRA training.
    ///
    /// In-process Rust LoRA training is not implemented because ONNX Runtime is inference-only
    /// and cannot be used for gradient-based weight updates. The full training pipeline uses a
    /// separate Python script (`scripts/train_lora.py`) that loads the base model via PyTorch +
    /// PEFT, trains LoRA adapters, and exports them as safetensors.
    ///
    /// Adapter re-loading back into ONNX after training is tracked in:
    ///   GitHub Issue #1 — https://github.com/darwin-finch/finch/issues/1
    async fn run_background_training(
        coordinator: Arc<TrainingCoordinator>,
        _models_dir: Option<PathBuf>,
    ) -> Result<BackgroundTrainingStats> {
        tracing::info!("Exporting training examples to offline queue");

        // Verify the buffer has examples before writing
        let num_examples = {
            let buffer = coordinator.buffer()?;
            let n = buffer.examples().len();
            if n == 0 {
                anyhow::bail!("No training examples in buffer");
            }
            n
        };

        // Write examples to ~/.finch/training_queue.jsonl for the Python training pipeline.
        // This is the only part of the training flow that works end-to-end in Rust today.
        coordinator.write_training_queue()?;

        let queue_path = dirs::home_dir()
            .map(|h| {
                h.join(".finch")
                    .join("training_queue.jsonl")
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| "~/.finch/training_queue.jsonl".to_string());

        tracing::info!("Queued {} examples → {}", num_examples, queue_path);

        Ok(BackgroundTrainingStats {
            examples_trained: num_examples,
            queue_path,
        })
    }

    /// Handle /plan command - enter planning mode
    async fn handle_plan_command(&mut self, task: String) -> Result<()> {
        use chrono::Utc;

        // Check if already in plan mode
        if matches!(
            self.mode,
            ReplMode::Planning { .. } | ReplMode::Executing { .. }
        ) {
            self.output_status(format!(
                "⚠️  Already in {} mode. Finish current task first.",
                match self.mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                }
            ));
            return Ok(());
        }

        // Create plans directory
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        // Generate plan filename
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to planning mode
        self.mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };

        self.output_status(format!("{}", "✓ Entered planning mode".blue().bold()));
        self.output_status(format!("📋 Task: {}", task));
        self.output_status(format!("📁 Plan will be saved to: {}", plan_path.display()));
        self.output_status("");
        self.output_status(format!("{}", "Available tools:".green()));
        self.output_status("  read, glob, grep, web_fetch");
        self.output_status(format!("{}", "Blocked tools:".red()));
        self.output_status("  bash, save_and_exec");
        self.output_status("");
        self.output_status("Ask me to explore the codebase and generate a plan.");
        self.output_status(format!(
            "{}",
            "Type /show-plan to view, /approve to execute, /reject to cancel.".dark_grey()
        ));

        // Add mode change notification to conversation
        self.conversation.write().await.add_user_message(format!(
            "[System: Entered planning mode for task: {}]\n\
             Available tools: read, glob, grep, web_fetch\n\
             Blocked tools: bash, save_and_exec\n\
             Please explore the codebase and generate a detailed plan.",
            task
        ));

        if self.is_interactive {
            self.output_status("");
            self.print_status_line().await;
        }

        Ok(())
    }
}
