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
    AnalyzeModelTool, BashTool, CompareResponsesTool, GenerateTrainingDataTool, GlobTool, GrepTool,
    QueryLocalModelTool, ReadTool, RestartTool, SaveAndExecTool, TrainTool, WebFetchTool,
};
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
use crate::{output_error, output_progress, output_status};

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

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    // Teacher session with context optimization
    teacher_session: Arc<RwLock<TeacherSession>>,
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
}

/// Background training statistics
struct BackgroundTrainingStats {
    examples_trained: usize,
    final_loss: f64,
    adapter_path: String,
}

impl Repl {
    pub async fn new(
        config: Config,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
    ) -> Self {
        // Detect if we're in interactive mode (stdout is a TTY)
        let is_interactive = io::stdout().is_terminal();

        // Set up models directory
        let models_dir = dirs::home_dir().map(|home| home.join(".shammah").join("models"));

        // Load validator only (router is now in Router)
        let threshold_validator = Self::load_validator(models_dir.as_ref(), is_interactive);

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

        // Initialize tool execution system
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(ReadTool));
        tool_registry.register(Box::new(GlobTool));
        tool_registry.register(Box::new(GrepTool));
        tool_registry.register(Box::new(WebFetchTool::new()));
        tool_registry.register(Box::new(BashTool));

        // Self-improvement tools
        let session_state_file = dirs::home_dir()
            .map(|home| home.join(".shammah").join("restart_state.json"))
            .unwrap_or_else(|| PathBuf::from(".shammah/restart_state.json"));

        tool_registry.register(Box::new(RestartTool::new(session_state_file.clone())));
        tool_registry.register(Box::new(SaveAndExecTool::new(session_state_file.clone())));

        // Active learning tools (Phase 2)
        tool_registry.register(Box::new(QueryLocalModelTool));
        tool_registry.register(Box::new(CompareResponsesTool));
        tool_registry.register(Box::new(GenerateTrainingDataTool));
        tool_registry.register(Box::new(AnalyzeModelTool));
        tool_registry.register(Box::new(TrainTool));

        // Create permission manager (allow all for now)
        let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);

        // Determine patterns path
        let patterns_path = dirs::home_dir()
            .map(|home| home.join(".shammah").join("tool_patterns.json"))
            .unwrap_or_else(|| PathBuf::from(".shammah/tool_patterns.json"));

        // Create tool executor
        let tool_executor = Arc::new(tokio::sync::Mutex::new(ToolExecutor::new(tool_registry, permissions, patterns_path)
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
                fallback_registry.register(Box::new(QueryLocalModelTool));
                fallback_registry.register(Box::new(CompareResponsesTool));
                fallback_registry.register(Box::new(GenerateTrainingDataTool));
                fallback_registry.register(Box::new(AnalyzeModelTool));
                fallback_registry.register(Box::new(TrainTool));
                ToolExecutor::new(
                    fallback_registry,
                    PermissionManager::new().with_default_rule(PermissionRule::Allow),
                    std::env::temp_dir().join("shammah_patterns_fallback.json"),
                )
                .expect("Failed to create fallback tool executor")
            })
        ));

        if is_interactive {
            output_status!(
                "✓ Tool execution enabled ({} tools)",
                tool_executor.lock().await.registry().len()
            );
        }

        let streaming_enabled = config.streaming_enabled;

        // Generate tool definitions from registry
        let tool_definitions: Vec<ToolDefinition> = tool_executor
            .lock()
            .await
            .registry()
            .get_all_tools()
            .into_iter()
            .map(|tool| tool.definition())
            .collect();

        // Get global OutputManager and StatusBar (created in main.rs)
        // DO NOT create new instances - that would break stdout control!
        use crate::cli::global_output::{global_output, global_status};
        let output_manager_arc = global_output();
        let output_manager = (*output_manager_arc).clone(); // Clone for local reference
        let status_bar_arc = global_status();
        let status_bar = (*status_bar_arc).clone();

        // CRITICAL: If TUI will be enabled, disable stdout NOW (before bootstrap spawns)
        // This prevents tracing logs from download.rs from writing directly to stdout
        // and overlapping with TUI content.
        if config.tui_enabled && is_interactive {
            output_manager_arc.disable_stdout();
        }

        // Initialize tokenizer
        let tokenizer = Arc::new(TextTokenizer::default().unwrap_or_else(|e| {
            output_status!("⚠️  Failed to create tokenizer: {}", e);
            output_status!("   Active learning tools may not work correctly");
            panic!("Cannot create tokenizer")
        }));

        // Initialize BootstrapLoader for progressive Qwen model loading
        let generator_state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let bootstrap_loader = Arc::new(BootstrapLoader::new(
            Arc::clone(&generator_state),
            Some(Arc::new(output_manager.clone())),
        ));

        // Check HuggingFace token early (warn if missing)
        let token_path = dirs::cache_dir()
            .and_then(|p| Some(p.join("huggingface").join("token")));

        if let Some(path) = token_path {
            if !path.exists() && is_interactive {
                output_progress!(
                    "⚠️  HuggingFace token not found at {:?}\n\
                     Local Qwen model downloads will fail.\n\
                     See README.md for setup instructions.\n\
                     Queries will forward to Claude API.",
                    path
                );
            }
        }

        if is_interactive {
            output_progress!("⏳ Initializing Qwen model (background)...");
        }

        // Start background model loading
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
        let tok_clone = Arc::clone(&tokenizer);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                let state = state_monitor.read().await;
                if let GeneratorState::Ready { model, .. } = &*state {
                    // Inject Qwen model into LocalGenerator
                    let mut gen = gen_clone.write().await;
                    *gen = LocalGenerator::with_models(
                        Some(Arc::clone(model)),
                        Some(Arc::clone(&tok_clone)),
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

        let sampling_config = SamplingConfig::default(); // 5% baseline, 3x arch, 5x security
        let sampler = Arc::new(RwLock::new(Sampler::new(sampling_config)));

        if is_interactive {
            output_status!("✓ LoRA fine-tuning enabled (weighted training)");
        }

        // Initialize TUI renderer if enabled (Phase 2: Ratatui interface)
        // Moved to global for Phase 5 native ratatui dialogs
        if config.tui_enabled && is_interactive {
            match TuiRenderer::new(Arc::new(output_manager.clone()), Arc::new(status_bar.clone())) {
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

        if is_interactive {
            output_status!("✓ Teacher context optimization enabled (Level 3)");
        }

        Self {
            _config: config,
            claude_client,
            teacher_session,
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
        tokenizer: Arc<TextTokenizer>,
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
                        return LocalGenerator::with_models(neural_generator, Some(tokenizer));
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
        LocalGenerator::with_models(neural_generator, Some(tokenizer))
    }

    /// Load validator from disk or create new one
    fn load_validator(models_dir: Option<&PathBuf>, is_interactive: bool) -> ThresholdValidator {
        let Some(models_dir) = models_dir else {
            return ThresholdValidator::new();
        };

        let validator_path = models_dir.join("threshold_validator.json");
        if validator_path.exists() {
            match ThresholdValidator::load(&validator_path) {
                Ok(validator) => {
                    if is_interactive {
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

    /// Output a Claude response (dual: buffer + stdout, or TUI only)
    fn output_claude(&self, content: impl Into<String> + Clone) {
        let content_str = content.clone().into();
        self.output_manager.write_claude(content_str.clone());
        // Only print to stdout if TUI is not active
        if self.is_interactive && !self.is_tui_active() {
            println!("{}", content_str);
        }
    }

    /// Append to the last Claude response (for streaming)
    fn output_claude_append(&self, content: impl AsRef<str>) {
        let content_str = content.as_ref();
        self.output_manager.append_claude(content_str);
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

                // Check if pre-approved in cache
                let approval_source = self.tool_executor.lock().await.is_approved(&signature);

                match approval_source {
                    ApprovalSource::NotApproved => {
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
                    self.output_claude(format!("    Claude: {}", assistant_text));
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

        // Start a new Claude response in the output buffer
        if full_response.is_empty() {
            self.output_manager.write_claude(String::new());
        }

        while let Some(result) = rx.recv().await {
            match result {
                Ok(crate::generators::StreamChunk::TextDelta(text_chunk)) => {
                    full_response.push_str(&text_chunk);

                    // Update the output buffer with the chunk
                    self.output_manager.append_claude(text_chunk.clone());

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
                self.output_claude(&response);
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
        ));

        // Get generator state from bootstrap loader
        let generator_state = Arc::clone(self.bootstrap_loader.state());

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
        );

        // Run the event loop
        event_loop.run().await
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.is_interactive {
            // Fancy startup for interactive mode
            self.output_status("Shammah v0.1.0 - Constitutional AI Proxy");
            self.output_status("Using API key from: ~/.shammah/config.toml ✓");
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
                let handler = self.input_handler.as_mut().unwrap();
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
                    Command::Approve => {
                        self.handle_approve_command().await?;
                        continue;
                    }
                    Command::Reject => {
                        self.handle_reject_command().await?;
                        continue;
                    }
                    Command::ShowPlan => {
                        self.handle_show_plan_command().await?;
                        continue;
                    }
                    Command::SavePlan => {
                        self.handle_save_plan_command().await?;
                        continue;
                    }
                    Command::Done => {
                        self.handle_done_command().await?;
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
                    self.output_claude(&response);
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

        // Add user message to conversation history
        self.conversation.write().await.add_user_message(query.to_string());

        if self.is_interactive {
            print!("{}", "Analyzing...".dark_grey());
            io::stdout().flush()?;
        }

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
                    match gen.try_generate(query) {
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
                        ForwardReason::Crisis => {
                            self.output_status("⚠️  CRISIS DETECTED");
                            self.output_status("→ Routing: FORWARDING TO CLAUDE");
                        }
                        ForwardReason::ModelNotReady => {
                            let status_msg = {
                                let state = self.bootstrap_loader.state().read().await;
                                state.status_message()
                            }; // Drop the state guard here
                            self.output_status("✓ Crisis check: PASS");
                            self.output_status(format!("ℹ️  Model status: {}", status_msg));
                            self.output_status("→ Routing: FORWARDING TO CLAUDE");
                        }
                        _ => {
                            self.output_status("✓ Crisis check: PASS");
                            self.output_status("✗ Threshold check: FAIL (confidence too low)");
                            self.output_status("→ Routing: FORWARDING TO CLAUDE");
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
            .await
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
        let buffer = self.training_coordinator.buffer().await;
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
                        output_status!("\n✓ Background training completed!");
                        output_status!("   Trained on {} examples", stats.examples_trained);
                        output_status!("   Final loss: {:.4}", stats.final_loss);
                        output_status!("   Adapter saved to: {}", stats.adapter_path);
                    }
                    Err(e) => {
                        output_error!("\n⚠️  Background training failed: {}", e);
                    }
                }
            });

            self.output_status("   Training started in background...");
        }

        Ok(())
    }

    /// Run LoRA training in background
    async fn run_background_training(
        coordinator: Arc<TrainingCoordinator>,
        models_dir: Option<PathBuf>,
    ) -> Result<BackgroundTrainingStats> {
        use crate::models::{LoRAAdapter, LoRAConfig, LoRATrainer};
        use std::sync::Arc as StdArc;

        tracing::info!("Starting background LoRA training");

        // Get training examples from buffer
        let examples = {
            let buffer = coordinator.buffer().await;
            buffer.examples().to_vec()
        };

        if examples.is_empty() {
            anyhow::bail!("No training examples in buffer");
        }

        let num_examples = examples.len();
        tracing::info!("Training on {} examples", num_examples);

        // Create LoRA configuration
        let lora_config = LoRAConfig {
            rank: 16,
            alpha: 32.0,
            dropout: 0.1,
            target_modules: vec!["q_proj".to_string(), "v_proj".to_string()],
        };

        // Determine device
        use crate::models::{get_device_with_preference, DevicePreference};
        let device = get_device_with_preference(DevicePreference::Auto)?;

        // Create LoRA adapter
        let adapter = LoRAAdapter::new(lora_config.clone(), device.clone())?;

        // Create tokenizer
        // TODO: Get tokenizer from actual Qwen model for production
        // For now, use a simple GPT-2 tokenizer as placeholder
        let tokenizer = StdArc::new({
            use tokenizers::models::bpe::BPE;
            let bpe = BPE::default();
            tokenizers::Tokenizer::new(bpe)
        });

        // Create trainer
        let mut trainer = LoRATrainer::new(
            adapter, tokenizer, 1e-4, // learning_rate
            4,    // batch_size
            3,    // epochs
        );

        // Convert WeightedExample to ExampleBuffer
        use crate::models::ExampleBuffer;
        let mut buffer = ExampleBuffer::new(examples.len());
        for example in examples {
            buffer.add(example);
        }

        // Train the adapter
        tracing::info!("Starting LoRA training...");
        let training_stats = trainer.train(&buffer)?;

        let final_loss = training_stats.last().map(|s| s.loss).unwrap_or(0.0);

        // Save adapter weights
        let adapters_dir = if let Some(ref dir) = models_dir {
            dir.parent().unwrap().join("adapters")
        } else {
            dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
                .join(".shammah")
                .join("adapters")
        };

        std::fs::create_dir_all(&adapters_dir)?;

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let adapter_filename = format!("lora_adapter_{}.safetensors", timestamp);
        let adapter_path = adapters_dir.join(&adapter_filename);

        trainer.adapter().save(&adapter_path)?;

        tracing::info!(
            "LoRA training completed. Adapter saved to: {}",
            adapter_path.display()
        );

        Ok(BackgroundTrainingStats {
            examples_trained: num_examples,
            final_loss,
            adapter_path: adapter_path.display().to_string(),
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
            .join(".shammah")
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

    /// Handle /approve command - approve plan and start execution
    async fn handle_approve_command(&mut self) -> Result<()> {
        use crate::cli::menu::{Menu, MenuOption};
        use chrono::Utc;

        match &self.mode {
            ReplMode::Planning {
                task, plan_path, ..
            } => {
                // Clone values we need before mutating self.mode
                let task_clone = task.clone();
                let plan_path_clone = plan_path.clone();

                self.output_status(format!("{}", "✓ Plan approved!".green().bold()));
                self.output_status("");

                // Show context clearing options
                self.output_status(format!("The plan has been saved to: {}", plan_path_clone.display()));
                self.output_status("");
                self.output_status("Would you like to:");
                self.output_status("  1. Clear conversation and execute plan (recommended)");
                self.output_status("  2. Keep conversation history and execute");
                self.output_status("");

                let options = vec![
                    MenuOption::with_description(
                        "Clear context (recommended)",
                        "Reduces token usage and focuses execution on the plan",
                        true,
                    ),
                    MenuOption::with_description(
                        "Keep context",
                        "Preserves exploration history in conversation",
                        false,
                    ),
                ];

                let clear_context = Menu::select(
                    "Choose execution mode:",
                    options,
                    Some("[↑↓ or j/k to move, Enter to select, or type 1-2]"),
                )?;

                // Transition to executing mode
                self.mode = ReplMode::Executing {
                    task: task_clone,
                    plan_path: plan_path_clone.clone(),
                    approved_at: Utc::now(),
                };

                if clear_context {
                    // Clear conversation and add plan as context
                    self.output_status("");
                    self.output_status(format!("{}", "Clearing conversation context...".blue()));
                    self.conversation.write().await.clear();

                    // Read plan file and add as initial context
                    if plan_path_clone.exists() {
                        let plan_content = std::fs::read_to_string(&plan_path_clone)
                            .context("Failed to read plan file")?;
                        self.conversation.write().await.add_user_message(format!(
                            "Please execute this plan:\n\n{}",
                            plan_content
                        ));
                        self.output_status(format!(
                            "{}",
                            "✓ Context cleared. Plan loaded as primary reference.".green()
                        ));
                    } else {
                        self.output_status(format!(
                            "{}",
                            "⚠️  Plan file not found. Adding approval message only.".yellow()
                        ));
                        self.conversation.write().await.add_user_message(
                            "[System: Plan approved! All tools are now enabled. \
                             You may execute bash commands and modify files.]"
                                .to_string(),
                        );
                    }
                } else {
                    // Keep history, just add approval message
                    self.output_status("");
                    self.output_status(format!("{}", "Keeping conversation context...".blue()));
                    self.conversation.write().await.add_user_message(
                        "[System: Plan approved! All tools are now enabled. \
                         You may execute bash commands and modify files.]"
                            .to_string(),
                    );
                }

                self.output_status("");
                self.output_status(format!(
                    "{}",
                    "All tools enabled. Please proceed with implementation.".green()
                ));

                if self.is_interactive {
                    self.output_status("");
                    self.print_status_line().await;
                }
            }
            ReplMode::Normal => {
                self.output_status("⚠️  No plan to approve. Use /plan first.");
            }
            ReplMode::Executing { .. } => {
                self.output_status("⚠️  Already executing plan.");
            }
        }
        Ok(())
    }

    /// Handle /reject command - reject plan and return to normal mode
    async fn handle_reject_command(&mut self) -> Result<()> {
        match &self.mode {
            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                self.output_status(format!("{}", "✗ Plan rejected. Returning to normal mode.".yellow()));
                self.mode = ReplMode::Normal;
                self.conversation.write().await.add_user_message("[System: Plan rejected by user.]".to_string());

                if self.is_interactive {
                    self.output_status("");
                    self.print_status_line().await;
                }
            }
            ReplMode::Normal => {
                self.output_status("⚠️  No active plan to reject.");
            }
        }
        Ok(())
    }

    /// Handle /show-plan command - display current plan
    async fn handle_show_plan_command(&mut self) -> Result<()> {
        match &self.mode {
            ReplMode::Planning { plan_path, .. } | ReplMode::Executing { plan_path, .. } => {
                if plan_path.exists() {
                    let content = std::fs::read_to_string(plan_path)?;
                    self.output_status(format!("\n{}", "=".repeat(60)));
                    self.output_status("PLAN:");
                    self.output_status(format!("{}", "=".repeat(60)));
                    self.output_status(format!("{}", content));
                    self.output_status(format!("{}", "=".repeat(60)));
                } else {
                    self.output_status("⚠️  Plan file not yet created.");
                }
            }
            ReplMode::Normal => {
                self.output_status("⚠️  No active plan. Use /plan to start.");
            }
        }
        Ok(())
    }

    /// Handle /save-plan command - manually save current response as plan
    async fn handle_save_plan_command(&mut self) -> Result<()> {
        // Get the last assistant message from conversation
        let messages = self.conversation.read().await.get_messages();
        let last_assistant_msg = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "assistant")
            .map(|msg| msg.text());

        if let Some(content) = last_assistant_msg {
            match &self.mode {
                ReplMode::Planning { plan_path, .. } => {
                    std::fs::write(plan_path, &content)?;
                    self.output_status(format!("✓ Plan saved to: {}", plan_path.display()));
                }
                ReplMode::Normal | ReplMode::Executing { .. } => {
                    // Create a new plan file
                    let plans_dir = dirs::home_dir()
                        .map(|home| home.join(".shammah").join("plans"))
                        .unwrap_or_else(|| PathBuf::from(".shammah/plans"));
                    std::fs::create_dir_all(&plans_dir)?;

                    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                    let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));
                    std::fs::write(&plan_path, &content)?;
                    self.output_status(format!("✓ Plan saved to: {}", plan_path.display()));
                }
            }
        } else {
            self.output_status(
                "⚠️  No assistant response to save. Please ask Claude to generate a plan first."
            );
        }
        Ok(())
    }

    /// Handle /done command - exit execution mode
    async fn handle_done_command(&mut self) -> Result<()> {
        match &self.mode {
            ReplMode::Executing { .. } => {
                self.output_status("✓ Plan execution complete. Returning to normal mode.");
                self.mode = ReplMode::Normal;
            }
            ReplMode::Planning { .. } => {
                self.output_status(
                    "⚠️  Currently in planning mode. Use /approve to execute or /reject to cancel."
                );
            }
            ReplMode::Normal => {
                self.output_status("⚠️  Not in execution mode.");
            }
        }
        Ok(())
    }
}
