# models — public interface

Generated from [`src/models/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/models/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Generation configuration parameters Exported as `AdapterGenerationConfig`.
pub struct GenerationConfig { … }
/// Registry for looking up adapters by model name
pub struct AdapterRegistry;
impl AdapterRegistry {
    /// Get adapter from model family enum
    pub fn from_family(family: ModelFamily) -> Box<dyn LocalModelAdapter>;
    /// Get appropriate adapter for a model by name
    pub fn get_adapter(model_name: &str) -> Box<dyn LocalModelAdapter>;
}
/// Background task that loads generator asynchronously
pub struct BootstrapLoader { … }
impl BootstrapLoader {
    /// Handle loading errors gracefully
    pub async fn handle_error(&self, error: anyhow::Error);
    /// Load generator in background using UnifiedModelLoader
    pub async fn load_generator_async(&self, provider: super::unified_loader::InferenceProvider, model_family: ModelFamily, model_size: ModelSize, execution_target: ExecutionTarget, coreml: crate::config::CoreMlConfig, model_repo: Option<String>) -> Result<()>;
    /// Set state to not available (offline mode)
    pub async fn set_not_available(&self);
    /// Create new bootstrap loader with shared state
    pub fn new(state: Arc<RwLock<GeneratorState>>, output: Option<Arc<dyn ModelProgress>>) -> Self;
    /// Get reference to the generator state
    pub fn state(&self) -> &Arc<RwLock<GeneratorState>>;
}
/// Result of comparing local vs Claude responses
pub struct ComparisonResult { … }
impl ComparisonResult {
    /// Check if responses differ significantly
    pub fn differ_significantly(&self) -> bool;
    /// Create from responses
    pub fn new(local_response: String, claude_response: String) -> Self;
}
/// Adapter for DeepSeek model family (DeepSeek-Coder, DeepSeek-V2, etc.)
pub struct DeepSeekAdapter;
/// Device configuration options (DEPRECATED: Phase 4 - kept for compatibility)  With ONNX Runtime, device selection is handled by execution providers: - CoreML…
pub enum DevicePreference { Auto, Cpu, Metal }
/// Download progress events sent via channel
pub enum DownloadProgress { Starting, Downloading, Complete, Error }
/// Snapshot of download progress for state updates
pub struct DownloadProgressSnapshot { … }
/// Example buffer for batching (stub for Phase 5)
pub struct ExampleBuffer { … }
impl ExampleBuffer {
    pub fn add(&mut self, example: WeightedExample);
    pub fn as_slice(&self) -> &[WeightedExample];
    pub fn examples(&self) -> &[WeightedExample];
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
    pub fn new(_capacity: usize) -> Self;
    pub fn total_weight(&self) -> f64;
}
/// Generator configuration - supports both custom and pre-trained models
pub enum GeneratorConfig { RandomInit, Pretrained }
/// Unified generator model supporting multiple backends
pub struct GeneratorModel { … }
impl GeneratorModel {
    /// Get mutable reference to backend (for accessing ONNX model directly)
    pub fn backend_mut(&mut self) -> &mut dyn TextGeneration;
    /// Get configuration
    pub fn config(&self) -> &GeneratorConfig;
    /// Fine-tune model with LoRA adapter (placeholder for future functionality)  # Arguments * `examples` - Training data as (query, response) pairs * `lora_config`…
    pub fn fine_tune(&mut self, _examples: &[(String, String)], _lora_config: crate::models::lora::LoRAConfig, _epochs: usize, _learning_rate: f64) -> Result<()>;
    /// Generate response from input tokens
    pub fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>>;
    /// Generate a text response from a text prompt.
    pub fn generate_text(&mut self, prompt: &str, max_new_tokens: usize) -> Result<String>;
    /// Load LoRA adapter weights (placeholder)
    pub fn load_lora(&mut self, _path: &Path) -> Result<()>;
    /// Get generator backend name
    pub fn name(&self) -> &str;
    /// Create new generator from configuration  Phase 4: Only supports Pretrained (ONNX-based) RandomInit removed with Candle
    pub fn new(config: GeneratorConfig) -> Result<Self>;
    /// Save LoRA adapter weights (placeholder)
    pub fn save_lora(&self, _path: &Path) -> Result<()>;
}
/// Generator loading state for progressive bootstrap
pub enum GeneratorState { Initializing, Downloading, Loading, Ready, Failed, NotAvailable }
impl GeneratorState {
    /// Check if generator is ready for use
    pub fn is_ready(&self) -> bool;
    /// Get human-readable status message
    pub fn status_message(&self) -> String;
}
/// Inference provider selection
pub enum InferenceProvider { Onnx, Candle }
impl InferenceProvider {
    /// Get description for users
    pub fn description(&self) -> &'static str;
    /// Get human-readable name
    pub fn name(&self) -> &'static str;
}
/// Adapter for Llama model family (Llama 3+ format)
pub struct LlamaAdapter;
/// LoRA adapter configuration  LoRA enables efficient fine-tuning of large models by learning low-rank updates to weight matrices.
pub struct LoRAConfig { … }
/// LoRA trainer (stub for Phase 5)
pub struct LoRATrainer { … }
impl LoRATrainer {
    pub fn adapter(&self) -> Result<&LoRATrainingAdapter>;
    pub fn new(adapter: LoRATrainingAdapter, _tokenizer: std::sync::Arc<tokenizers::Tokenizer>, learning_rate: f64, batch_size: usize, epochs: usize) -> Self;
    pub fn train(&mut self, _examples: &ExampleBuffer) -> Result<Vec<TrainingStats>>;
}
/// LoRA adapter for fine-tuning pre-trained models  # Example (Future Usage) ```text use finch::models::{GeneratorModel, LoRATrainingAdapter, LoRAConfig};  // L…
pub struct LoRATrainingAdapter { … }
impl LoRATrainingAdapter {
    /// Get adapter configuration
    pub fn config(&self) -> &LoRAConfig;
    /// Create LoRA adapter with default configuration
    pub fn default_config() -> Result<Self>;
    /// Disable the LoRA adapter (revert to base model)
    pub fn disable(&mut self);
    /// Enable the LoRA adapter
    pub fn enable(&mut self);
    /// Check if adapter is enabled
    pub fn is_enabled(&self) -> bool;
    /// Load adapter weights from file  # Future Implementation Will load previously trained adapter from safetensors format
    pub fn load(_path: &std::path::Path) -> Result<Self>;
    /// Create new LoRA adapter with given configuration Phase 4: device parameter removed (was Candle-based)
    pub fn new(config: LoRAConfig, _device: ()) -> Result<Self>;
    /// Save adapter weights to file  # Future Implementation Will save: - Low-rank matrices (A and B) - Configuration (rank, alpha, target modules) - Metadata (trai…
    pub fn save(&self, _path: &std::path::Path) -> Result<()>;
    /// Train LoRA adapter on examples  # Arguments * `examples` - Training data as (query, response) pairs * `epochs` - Number of training epochs (1-10 typical) * `…
    pub fn train(&mut self, _examples: &[(String, String)], _epochs: usize, _learning_rate: f64) -> Result<()>;
}
/// Loaded ONNX model with tokenizer
pub struct LoadedOnnxModel { … }
impl LoadedOnnxModel {
    /// Generate text from prompt  NOTE: This is a placeholder for Phase 2.
    pub fn generate(&self, prompt: &str, _max_tokens: usize) -> Result<String>;
    /// Get model name
    pub fn model_name(&self) -> &str;
    /// Get model path
    pub fn model_path(&self) -> &Path;
    /// Get model size
    pub fn model_size(&self) -> ModelSize;
    /// Get tokenizer reference
    pub fn tokenizer(&self) -> &Tokenizer;
}
/// Adapter for Mistral model family
pub struct MistralAdapter;
/// Model compatibility information
pub struct ModelCompatibility { … }
impl ModelCompatibility {
    /// Get repository ID for a specific size and provider
    pub fn get_repository(&self, provider: InferenceProvider, size: ModelSize) -> Option<String>;
}
/// Common model configuration (for custom transformers)
pub struct ModelConfig { … }
impl ModelConfig {
    /// Create config optimized for Apple Silicon
    pub fn for_apple_silicon() -> Self;
    /// Create small config for fast testing (works well on CPU)
    pub fn small() -> Self;
}
/// Model downloader with HuggingFace Hub integration
pub struct ModelDownloader { … }
impl ModelDownloader {
    /// Get cache directory path
    pub fn cache_dir(&self) -> PathBuf;
    /// Download model with progress tracking (generic for any model family)  Returns path to cached model directory containing safetensors and tokenizer files.
    pub fn download_model(&self, repo_id: &str, estimated_size_gb: f64) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)>;
    /// Download Qwen model with progress tracking (convenience wrapper)  Returns path to cached model directory containing safetensors and tokenizer files.
    pub fn download_qwen_model(&self, model_size: QwenSize) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)>;
    /// Check if model is already cached
    pub fn is_cached(&self, model_size: QwenSize) -> bool;
    /// Create new downloader (uses default HF cache: ~/.cache/huggingface/)
    pub fn new() -> Result<Self>;
    /// Create downloader with custom cache directory
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self>;
}
/// Expected output for training
pub enum ModelExpectation { RouteDecision, PatternLabel, ResponseTarget, QualityTarget }
/// Supported model families
pub enum ModelFamily { Qwen2, Gemma2, Llama3, Mistral, Phi, DeepSeek }
impl ModelFamily {
    /// Get description for users
    pub fn description(&self) -> &'static str;
    pub fn from_name(name: &str) -> Option<Self>;
    /// Get human-readable name
    pub fn name(&self) -> &'static str;
}
/// Configuration for loading any model on any execution target with any provider
pub struct ModelLoadConfig { … }
impl ModelLoadConfig {
    /// Legacy field accessor for backward compatibility
    pub fn backend(&self) -> ExecutionTarget;
    /// Describe the requested target policy without implying observed placement.
    pub fn requested_target_name(&self) -> String;
}
/// Coordinates training and inference across all models
pub struct ModelManager { … }
impl ModelManager {
    /// Get models directory
    pub fn models_dir(&self) -> &Path;
    /// Create new model manager
    pub fn new(models_dir: PathBuf) -> Self;
    /// Get overall training statistics
    pub fn overall_stats(&self) -> OverallStats;
    /// Get recent training events for a model
    pub fn recent_events(&self, model_name: &str, limit: usize) -> Vec<&TrainingEvent>;
    /// Record a training event
    pub fn record_training(&mut self, model_name: String, query: String, prediction: Option<ModelPrediction>, expectation: String, success: bool);
    /// Train multiple models from a single query-response pair
    pub fn train_from_query_response(&mut self, query: &str, _response: &str, routing_was_local: bool, was_successful: bool) -> Result<TrainingReport>;
}
/// Metadata saved alongside model weights
pub struct ModelMetadata { … }
impl ModelMetadata {
    pub fn new(config: ModelConfig, model_type: String, training_step: usize) -> Self;
}
/// Prediction from a model
pub struct ModelPrediction { … }
/// Result of model selection — either a specific model or cloud-only mode
pub enum ModelSelection { Local, CloudOnly }
impl ModelSelection {
    pub fn is_cloud_only(&self) -> bool;
    pub fn model(&self) -> Option<QwenSize>;
}
/// Model selection based on system resources
pub struct ModelSelector;
impl ModelSelector {
    /// Get total system RAM in GB.
    pub fn get_total_ram_gb() -> usize;
    /// Select appropriate model based on available system RAM.
    pub fn select_for_system() -> Result<ModelSelection>;
    /// Backwards-compat wrapper — returns the Qwen variant or defaults to 1.5B in cloud-only
    pub fn select_model_for_system() -> Result<QwenSize>;
    /// Select model with manual override
    pub fn select_model_with_override(override_size: Option<QwenSize>) -> Result<QwenSize>;
}
/// Model size categories (family-specific)
pub enum ModelSize { Small, Medium, Large, XLarge }
impl ModelSize {
    /// Convert legacy QwenSize to generic ModelSize
    pub fn from_qwen(qwen_size: QwenSize) -> Self;
    /// Select appropriate size based on available RAM
    pub fn from_ram(ram_gb: usize) -> Result<Self>;
    /// Select appropriate model size based on available RAM
    pub fn from_ram(ram_gb: usize) -> Self;
    /// Get approximate RAM requirement in GB
    pub fn ram_requirement_gb(&self) -> usize;
    /// Convert to family-specific size string for repository resolution
    pub fn to_size_string(&self, family: ModelFamily) -> &'static str;
    /// Get model size string for HuggingFace model ID
    pub fn to_string(&self) -> &str;
}
/// Training statistics for a model
pub struct ModelStats { … }
/// ONNX sentence transformer embedding engine.
pub struct NeuralEmbeddingEngine { … }
impl NeuralEmbeddingEngine {
    /// Async version: download model using a blocking thread pool.
    pub async fn ensure_downloaded() -> Result<PathBuf>;
    /// Download the embedding model from HuggingFace if not already cached.
    pub fn download_sync() -> Result<PathBuf>;
    /// Try to find the model in the HuggingFace cache without downloading.
    pub fn find_in_cache() -> Option<PathBuf>;
    /// Load a pre-downloaded embedding model from a directory.
    pub fn load(model_dir: &Path) -> Result<Self>;
}
/// Overall training statistics
pub struct OverallStats { … }
/// Adapter for Phi model family (Phi-2, Phi-3, Phi-3.5)
pub struct PhiAdapter;
/// Model-specific prediction data
pub enum PredictionData { Route, Pattern, Response, Quality }
/// Quality signals that can be measured heuristically
pub enum QualitySignal { TooShort, TooLong, Repetitive, Incomplete, NoContent, HasCode, WellFormatted, AnswersQuestion }
/// Categories of queries for sampling prioritization
pub enum QueryCategory { Architecture, Security, Performance, Testing, General }
impl QueryCategory {
    /// Detect category from query text
    pub fn from_query(query: &str) -> Self;
    /// Get sampling multiplier for this category
    pub fn sampling_multiplier(&self, config: &SamplingConfig) -> f64;
}
/// Adapter for Qwen model family (ChatML format)
pub struct QwenAdapter;
impl QwenAdapter {
    /// Static method for cleaning output without adapter instance This can be called from message rendering for streaming responses
    pub fn clean_output_static(raw_output: &str) -> String;
}
/// Qwen model size variants — ordered from smallest to largest
pub enum QwenSize { Qwen500M, Qwen1_5B, Qwen3B, Qwen7B, Qwen14B }
impl QwenSize {
    /// Get human-readable description
    pub fn description(&self) -> &'static str;
    /// Get approximate download size in GB
    pub fn download_size_gb(&self) -> f64;
    /// Get HuggingFace model ID for this variant
    pub fn model_id(&self) -> &'static str;
    /// Get ONNX community repository ID
    pub fn onnx_repo_id(&self) -> &'static str;
    /// Get approximate RAM requirement in GB
    pub fn ram_requirement_gb(&self) -> usize;
}
/// Sampler that decides when to send queries to Claude
pub struct Sampler { … }
impl Sampler {
    /// Get current config
    pub fn config(&self) -> &SamplingConfig;
    /// Create new sampler
    pub fn new(config: SamplingConfig) -> Self;
    /// Update config
    pub fn set_config(&mut self, config: SamplingConfig);
    /// Decide whether to sample this query
    pub fn should_sample(&mut self, query: &str, _confidence: Option<f64>) -> SamplingDecision;
}
/// Sampling configuration
pub struct SamplingConfig { … }
/// Sampling decision
pub struct SamplingDecision { … }
/// No-op sink used when no host is attached (daemon, tests, unattended download).
pub struct SilentModelProgress;
/// Text tokenizer (stub for compatibility)  Phase 4: This is a stub.
pub struct TextTokenizer;
impl TextTokenizer {
    pub fn decode(&self, _tokens: &[u32], _skip_special_tokens: bool) -> Result<String>;
    pub fn encode(&self, _text: &str, _add_special_tokens: bool) -> Result<Vec<u32>>;
    pub fn new(_vocab_size: usize) -> Result<Self>;
    pub fn stub() -> Result<Self>;
}
/// Query category for pattern matching Exported as `ThresholdQueryCategory`.
pub enum QueryCategory { Greeting, Definition, HowTo, Explanation, Code, Debugging, Comparison, Opinion, Other }
impl ThresholdQueryCategory {
    /// Detect category from query text
    pub fn from_query(query: &str) -> Self;
    /// Get sampling multiplier for this category
    pub fn sampling_multiplier(&self, config: &SamplingConfig) -> f64;
}
/// Threshold-based router using statistics
pub struct ThresholdRouter { … }
impl ThresholdRouter {
    /// Deprecated: Use learn_local_attempt() or learn_forwarded() instead This method is kept for backward compatibility but logs a warning
    pub fn learn(&mut self, query: &str, was_successful: bool);
    /// Learn from a forwarded query (called when we forwarded to Claude)
    pub fn learn_forwarded(&mut self, _query: &str);
    /// Learn from a local generation attempt (called only when we tried local)
    pub fn learn_local_attempt(&mut self, query: &str, was_successful: bool);
    /// Load router state from disk Generates a new session ID to represent this program run
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self>;
    /// Create new threshold router with balanced defaults
    pub fn new() -> Self;
    /// Save router state to disk Save router state with concurrent-safe merging Acquires exclusive lock, merges with existing state if from different session, write…
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()>;
    /// Decide whether to try local generation
    pub fn should_try_local(&self, query: &str) -> bool;
    /// Get statistics
    pub fn stats(&self) -> ThresholdRouterStats;
}
/// Statistics snapshot
pub struct ThresholdRouterStats { … }
/// Threshold-based validator
pub struct ThresholdValidator { … }
impl ThresholdValidator {
    /// Learn from actual validation result
    pub fn learn(&mut self, query: &str, response: &str, was_actually_good: bool);
    /// Load validator state
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self>;
    /// Create new threshold validator with conservative defaults
    pub fn new() -> Self;
    /// Calculate quality score without validation side effects Returns a score from 0.0 (very bad) to 1.0 (excellent)
    pub fn quality_score(&self, query: &str, response: &str) -> f64;
    /// Save validator state
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()>;
    /// Get statistics
    pub fn stats(&self) -> ValidatorStats;
    /// Validate a response quality (0 = bad, 1 = good)
    pub fn validate(&self, query: &str, response: &str) -> bool;
}
/// Text generation trait - abstraction over different generator backends Callback type for streaming generation
pub type TokenCallback = Box<dyn FnMut(u32, &str) + Send>;
/// Parser for extracting tool calls from model output
pub struct ToolCallParser;
impl ToolCallParser {
    /// Extract text content (everything outside tool_use tags)  Removes all <tool_use>...</tool_use> blocks and returns remaining text.
    pub fn extract_text(output: &str) -> String;
    /// Check if output contains any tool calls  Fast check without full parsing.
    pub fn has_tool_calls(output: &str) -> bool;
    /// Extract all tool uses from output  Parses XML-formatted tool_use blocks and creates ToolUse objects.
    pub fn parse(output: &str) -> Result<Vec<ToolUse>>;
}
/// Formats tool definitions and results for local model prompts
pub struct ToolPromptFormatter;
impl ToolPromptFormatter {
    /// Format tool results for continuation prompt  Creates a message showing tool execution results that prompts the model to continue based on the tool outputs.
    pub fn format_tool_results(results: &[ToolResult]) -> String;
    /// Format tool definitions into system prompt text  Creates a comprehensive system prompt that includes: - Tool usage instructions - Available tools with descri…
    pub fn format_tools_for_prompt(tools: &[ToolDefinition]) -> String;
}
/// Legacy manual training coordinator.
pub struct TrainingCoordinator { … }
impl TrainingCoordinator {
    /// Add example to buffer, returns true if training threshold reached
    pub fn add_example(&self, example: WeightedExample) -> Result<bool>;
    pub fn buffer(&self) -> Result<std::sync::RwLockReadGuard<'_, ExampleBuffer>>;
    /// Clear buffer after writing to queue
    pub fn clear_buffer(&self) -> Result<()>;
    pub fn new(buffer_size: usize, threshold: usize, auto_train: bool) -> Self;
    /// Get training queue path
    pub fn queue_path(&self) -> &std::path::Path;
    pub fn should_train(&self) -> bool;
    /// Placeholder for external training trigger
    pub fn train(&self) -> Result<()>;
    /// Construct a coordinator with an explicit queue destination.
    pub fn with_queue_path(buffer_size: usize, threshold: usize, auto_train: bool, queue_path: impl Into<std::path::PathBuf>) -> Self;
    /// Write buffered examples to JSONL queue file
    pub fn write_training_queue(&self) -> Result<usize>;
}
/// Report from a training cycle
pub struct TrainingReport { … }
/// Training stats (stub for Phase 5)
pub struct TrainingStats { … }
impl TrainingStats {
    pub fn new() -> Self;
}
/// Generic model loader supporting multiple families and backends
pub struct UnifiedModelLoader { … }
impl UnifiedModelLoader {
    /// Load model with configuration (supports both ONNX and Candle providers)
    pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>>;
    /// Load ONNX model (Phase 4: Primary loading method)  This will replace the Candle-based loaders in Phase 4.
    pub fn load_onnx(&self, ram_gb: Option<usize>) -> Result<LoadedOnnxModel>;
    /// Create new unified loader
    pub fn new() -> Result<Self>;
}
/// Statistics snapshot
pub struct ValidatorStats { … }
/// Weighted training example (Phase 6: Added serialization for JSONL export)
pub struct WeightedExample { … }
impl WeightedExample {
    pub fn critical(query: String, response: String, feedback: String) -> Self;
    pub fn improvement(query: String, response: String, feedback: String) -> Self;
    pub fn normal(query: String, response: String, feedback: String) -> Self;
    pub fn with_weight(query: String, response: String, feedback: String, weight: f64) -> Self;
}
```

## Traits

```rust
/// Determinate download progress handle returned by [`ModelProgress`].
pub trait DownloadProgressDisplay: Send + Sync {
    fn update(&self, current: u64);
    fn complete(&self);
    fn fail(&self);
}
/// Core trait for all learning models
pub trait LearningModel: Send + Sync {
    fn update(&mut self, input: &str, expected: &ModelExpectation) -> Result<()>;
    fn predict(&self, input: &str) -> Result<ModelPrediction>;
    fn save(&self, path: &Path) -> Result<()>;
    fn load(path: &Path) -> Result<Self> where Self: Sized;
    fn name(&self) -> &str;
    fn stats(&self) -> ModelStats;
}
/// Local model adapter for formatting prompts and handling model-specific behavior
pub trait LocalModelAdapter: Send + Sync {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String;
    fn eos_token_id(&self) -> u32;
    fn bos_token_id(&self) -> Option<u32>;
    fn clean_output(&self, raw_output: &str) -> String;
    fn family_name(&self) -> &str;
    fn generation_config(&self) -> GenerationConfig;
}
/// Host-supplied progress reporting for model bootstrap and download.
pub trait ModelProgress: Send + Sync {
    fn write_progress(&self, content: String);
    fn start_download_progress(&self, label: String, total: u64) -> Arc<dyn DownloadProgressDisplay>;
}
/// Model persistence
pub trait Saveable {
    fn save(&self, path: &Path) -> Result<()>;
    fn load(path: &Path) -> Result<Self> where Self: Sized;
}
pub trait TextGeneration: Send + Sync {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>>;
    fn generate_stream(&mut self, input_ids: &[u32], max_new_tokens: usize, _token_callback: TokenCallback) -> Result<Vec<u32>>;
    fn tokenize(&self, text: &str) -> Result<Vec<u32>>;
    fn decode_tokens(&self, tokens: &[u32]) -> Result<String>;
    fn name(&self) -> &str;
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
```

## Functions

```rust
/// Stub: Device info removed (Phase 4)
pub fn device_info() -> String { … }
/// Get available sizes for a model family
pub fn get_available_sizes(family: ModelFamily) -> Vec<ModelSize> { … }
/// Get all model families compatible with a given execution target
pub fn get_compatible_families(target: ExecutionTarget) -> Vec<ModelFamily> { … }
/// Stub: Device selection removed (Phase 4)
pub fn get_device_with_preference(_preference: DevicePreference) -> Result<()> { … }
/// Get repository ID for a specific provider, family, and size
pub fn get_repository(provider: InferenceProvider, family: ModelFamily, size: ModelSize) -> Option<String> { … }
/// Get supported execution targets for a model family
pub fn get_supported_targets(family: ModelFamily) -> Vec<ExecutionTarget> { … }
/// Install the process-wide download progress sink at a composition root.
pub fn install_model_progress(progress: Arc<dyn ModelProgress>) { … }
/// Check if a model family is compatible with an execution target
pub fn is_compatible(family: ModelFamily, target: ExecutionTarget) -> bool { … }
/// Stub: Metal availability check removed (Phase 4)
pub fn is_metal_available() -> bool { … }
/// Load model metadata
pub fn load_model_metadata(weights_path: &Path) -> Result<ModelMetadata> { … }
/// Check if a saved model exists
pub fn model_exists(weights_path: &Path) -> bool { … }
/// Save model with metadata (DEPRECATED: Phase 4 - Candle-based)  Phase 4: This function used Candle's VarMap which has been removed.
pub fn save_model_with_metadata(_weights_path: &Path, _varmap: &(), // Placeholder for removed VarMap type _metadata: &ModelMetadata) -> Result<()> { … }
/// Select the production memory embedding engine without downloading.
pub fn select_memory_embedding_engine(use_neural_embeddings: bool) -> Arc<dyn EmbeddingEngine> { … }
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `TrainingEvent`
