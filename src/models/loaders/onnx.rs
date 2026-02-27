use anyhow::{bail, Context, Result};
use ndarray;

/// Token streaming callback (receives token id + decoded string).
type TokenCallback = Box<dyn FnMut(u32, &str) + Send>;

/// Return type for a single forward pass: logits + updated KV cache.
type ForwardOutput = (Vec<f32>, Vec<(DynValue, DynValue)>);
use ort::{
    ep,
    memory::MemoryInfo,
    session::{builder::GraphOptimizationLevel, output::SessionOutputs, Session},
    value::{DynValue, Value},
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

use super::onnx_config::{ExecutionProvider as ConfigExecutionProvider, ModelSize, OnnxLoadConfig};
use crate::models::download::{DownloadProgress, ModelDownloader};
use crate::models::generator_new::TextGeneration;

/// ONNX model loader - downloads and loads models from HuggingFace
#[allow(dead_code)]
pub struct OnnxLoader {
    cache_dir: PathBuf,
}

impl OnnxLoader {
    /// Create new ONNX loader with cache directory
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Create ONNX Runtime session with execution providers
    fn create_session(&self, model_path: &Path, config: &OnnxLoadConfig) -> Result<Session> {
        info!("Creating ONNX session from: {:?}", model_path);

        // Suppress ONNX Runtime logs (set before session creation)
        // ORT_LOGGING_LEVEL: 0=Verbose, 1=Info, 2=Warning, 3=Error, 4=Fatal
        std::env::set_var("ORT_LOGGING_LEVEL", "2"); // Warning and above only

        // Build execution provider list
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(4)?; // Parallel ops within layer

        // Add execution providers based on config
        let providers = self.get_execution_providers(config);
        if !providers.is_empty() {
            builder = builder.with_execution_providers(providers)?;
        }

        // Create session
        let session = builder
            .commit_from_file(model_path)
            .context("Failed to create ONNX session")?;

        info!("ONNX session created successfully");

        Ok(session)
    }

    /// Get execution providers based on backend configuration
    fn get_execution_providers(
        &self,
        config: &OnnxLoadConfig,
    ) -> Vec<ort::ep::ExecutionProviderDispatch> {
        let mut providers = vec![];

        // Add execution providers based on config
        if let Some(exec_providers) = &config.execution_providers {
            for provider in exec_providers {
                match provider {
                    ConfigExecutionProvider::CoreML => {
                        #[cfg(target_os = "macos")]
                        {
                            info!("Requesting CoreML execution provider");
                            providers.push(ep::CoreML::default().build());
                        }
                    }
                    ConfigExecutionProvider::CUDA => {
                        #[cfg(feature = "cuda")]
                        {
                            info!("Requesting CUDA execution provider");
                            providers.push(ep::CUDA::default().build());
                        }
                    }
                    ConfigExecutionProvider::CPU => {
                        info!("Requesting CPU execution provider");
                        providers.push(ep::CPU::default().build());
                    }
                    ConfigExecutionProvider::TensorRT => {
                        #[cfg(feature = "cuda")]
                        {
                            info!("Requesting TensorRT execution provider");
                            providers.push(ep::TensorRT::default().build());
                        }
                    }
                    ConfigExecutionProvider::DirectML => {
                        #[cfg(target_os = "windows")]
                        {
                            info!("Requesting DirectML execution provider");
                            providers.push(ep::DirectML::default().build());
                        }
                    }
                }
            }
        } else {
            // Default: Try platform-specific providers first, then CPU
            #[cfg(target_os = "macos")]
            {
                info!("Auto-selecting: Trying CoreML");
                providers.push(ep::CoreML::default().build());
            }

            #[cfg(feature = "cuda")]
            {
                info!("Auto-selecting: Trying CUDA");
                providers.push(ep::CUDA::default().build());
            }
        }

        // Always add CPU as fallback
        info!("Adding CPU as fallback provider");
        providers.push(ep::CPU::default().build());

        providers
    }

    /// Load ONNX model with progress tracking
    pub fn load_model_sync(&self, config: &OnnxLoadConfig) -> Result<LoadedOnnxModel> {
        info!("Loading ONNX model: {}", config.model_name);

        // Step 1: Download model files from HuggingFace
        let (model_dir, _progress_rx) = self.download_model_files(config)?;

        // Step 2: Find model.onnx file
        // onnx-community repos store models in onnx/ subdirectory
        let onnx_subdir_path = model_dir.join("onnx").join("model.onnx");
        let root_path = model_dir.join("model.onnx");

        let model_path = if onnx_subdir_path.exists() {
            info!("Found ONNX model at: {:?}", onnx_subdir_path);
            onnx_subdir_path
        } else if root_path.exists() {
            info!("Found ONNX model at: {:?}", root_path);
            root_path
        } else {
            bail!(
                "ONNX model file not found.\n\
                 Tried:\n\
                 - {:?}\n\
                 - {:?}",
                onnx_subdir_path,
                root_path
            );
        };

        // Step 3: Load tokenizer
        let tokenizer = self.load_tokenizer(&model_dir)?;

        // Step 4: Create ONNX Runtime session
        let session = self.create_session(&model_path, config)?;

        info!("Successfully loaded ONNX model: {}", config.model_name);

        Ok(LoadedOnnxModel {
            session,
            tokenizer,
            model_name: config.model_name.clone(),
            model_size: config.size,
            model_path,
        })
    }

    /// Download model files from HuggingFace Hub
    fn download_model_files(
        &self,
        config: &OnnxLoadConfig,
    ) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)> {
        let repo = config.huggingface_repo();
        info!("Downloading from HuggingFace: {}", repo);

        let downloader = ModelDownloader::new()?;

        // Estimate size based on model size
        let estimated_size_gb = match config.size {
            ModelSize::Small => 0.5,
            ModelSize::Medium => 1.5,
            ModelSize::Large => 3.0,
            ModelSize::XLarge => 7.0,
        };

        // Download model files (model.onnx + model.onnx_data if exists)
        let (model_dir, progress_rx) = downloader
            .download_model(&repo, estimated_size_gb)
            .context("Failed to download ONNX model")?;

        Ok((model_dir, progress_rx))
    }

    /// Load tokenizer from model directory
    fn load_tokenizer(&self, model_dir: &Path) -> Result<Tokenizer> {
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !tokenizer_path.exists() {
            bail!("Tokenizer file not found: {:?}", tokenizer_path);
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!("Failed to load tokenizer from {:?}: {}", tokenizer_path, e)
        })?;

        debug!("Tokenizer loaded successfully");
        Ok(tokenizer)
    }
}

/// Loaded ONNX model with tokenizer
pub struct LoadedOnnxModel {
    session: Session,
    tokenizer: Tokenizer,
    model_name: String,
    model_size: ModelSize,
    model_path: PathBuf,
}

impl LoadedOnnxModel {
    /// Get model name
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Get model size
    pub fn model_size(&self) -> ModelSize {
        self.model_size
    }

    /// Generate text from prompt
    ///
    /// NOTE: This is a placeholder for Phase 2.
    /// Full implementation in Phase 3 will handle:
    /// - ONNX Runtime session creation and inference
    /// - Streaming generation
    /// - Proper sampling (temperature, top_p, etc.)
    /// - Attention masks and position IDs
    /// - KV cache management
    /// - Stop tokens
    pub fn generate(&self, prompt: &str, _max_tokens: usize) -> Result<String> {
        info!("Generating response for prompt (placeholder)");

        // Step 1: Tokenize input (verify tokenizer works)
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("Failed to encode prompt: {}", e))?;

        let input_ids = encoding.get_ids();
        debug!("Input tokens: {} tokens", input_ids.len());

        // For Phase 2, return placeholder indicating ONNX structure is in place
        warn!("ONNX generation not yet fully implemented - returning placeholder");
        Ok(format!(
            "[ONNX placeholder - model: {}, tokenized {} tokens]",
            self.model_name,
            input_ids.len()
        ))
    }

    /// Get tokenizer reference
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Get model path
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Autoregressive text generation with KV cache (Phase 5.1)
    fn generate_autoregressive(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
    ) -> Result<Vec<u32>> {
        self.generate_autoregressive_with_callback(input_ids, max_new_tokens, None)
    }

    /// Generate tokens autoregressively with optional streaming callback
    fn generate_autoregressive_with_callback(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        mut token_callback: Option<TokenCallback>,
    ) -> Result<Vec<u32>> {
        info!(
            "ONNX autoregressive generation: {} input tokens, max {} new tokens",
            input_ids.len(),
            max_new_tokens
        );

        let mut output_ids = input_ids.to_vec();
        let eos_token_id = self.get_eos_token_id();

        // Model architecture (from config.json)
        const NUM_LAYERS: usize = 28;
        const NUM_KV_HEADS: usize = 2;
        const HEAD_DIM: usize = 128; // hidden_size / num_attention_heads = 1536 / 12

        // Initialize empty KV cache for first step
        let mut past_key_values: Vec<(DynValue, DynValue)> = Vec::new();
        let mut past_seq_len = 0;

        // Generation loop
        for step in 0..max_new_tokens {
            debug!("Generation step {}/{}", step + 1, max_new_tokens);

            // 1. Prepare input tensor - only the new token(s) after first step
            let input_for_step = if step == 0 {
                &output_ids[..] // First step: all input tokens
            } else {
                &output_ids[output_ids.len() - 1..] // Subsequent: only last generated token
            };

            // 2. Run inference with KV cache using IoBinding
            let (logits, new_kv_cache) = self.run_with_kv_cache(
                input_for_step,
                &past_key_values,
                past_seq_len,
                NUM_LAYERS,
                NUM_KV_HEADS,
                HEAD_DIM,
            )?;

            // Update sequence length for next iteration
            past_seq_len += input_for_step.len();

            // Update KV cache for next iteration
            past_key_values = new_kv_cache;

            // 3. Sample next token with repetition penalty (pass previous output tokens)
            let previous_output = &output_ids[input_ids.len()..]; // Only new tokens, not input
            let next_token = Self::sample_token_with_params(
                &logits,
                previous_output,
                0.7,  // temperature: moderate randomness
                0.9,  // top_p: nucleus sampling
                1.15, // repetition_penalty: discourage repetition
            )?;
            debug!("Generated token: {}", next_token);

            // 4. Check for EOS
            if next_token == eos_token_id {
                info!("EOS token generated, stopping");
                break;
            }

            // 5. Append to output
            output_ids.push(next_token);

            // 6. Call streaming callback if provided
            if let Some(ref mut callback) = token_callback {
                // Decode just this token to text
                let token_text = self
                    .tokenizer
                    .decode(&[next_token], false)
                    .unwrap_or_else(|_| format!("[token_{}]", next_token));
                callback(next_token, &token_text);
            }
        }

        info!(
            "Generated {} new tokens",
            output_ids.len() - input_ids.len()
        );
        Ok(output_ids)
    }

    /// Run inference with KV cache using IoBinding for dynamic inputs
    fn run_with_kv_cache(
        &mut self,
        input_tokens: &[u32],
        past_kv: &[(DynValue, DynValue)],
        past_seq_len: usize,
        num_layers: usize,
        num_kv_heads: usize,
        head_dim: usize,
    ) -> Result<ForwardOutput> {
        // Prepare input_ids tensor
        let input_tensor = self.prepare_input(input_tokens)?;

        // Prepare position_ids tensor
        let position_ids = self.prepare_position_ids(input_tokens.len(), past_seq_len)?;

        // Prepare attention_mask tensor
        let attention_mask = self.prepare_attention_mask(input_tokens.len(), past_seq_len)?;

        // For first step, create empty KV cache tensors
        let kv_cache = if past_seq_len == 0 {
            // Empty cache: shape [1, num_kv_heads, 0, head_dim]
            let mut cache = Vec::new();
            for _ in 0..num_layers {
                let empty_key = ndarray::Array4::<f32>::zeros((1, num_kv_heads, 0, head_dim));
                let empty_value = ndarray::Array4::<f32>::zeros((1, num_kv_heads, 0, head_dim));

                let key_val = Value::from_array(empty_key)?.into_dyn();
                let value_val = Value::from_array(empty_value)?.into_dyn();

                cache.push((key_val, value_val));
            }
            cache
        } else {
            // Reuse existing cache from previous step (already owned Values)
            Vec::new() // Will bind past_kv directly below
        };

        // Create IoBinding for dynamic inputs
        let mut binding = self.session.create_binding()?;

        // Bind input_ids
        binding.bind_input("input_ids", &input_tensor)?;

        // Bind position_ids
        binding.bind_input("position_ids", &position_ids)?;

        // Bind attention_mask
        binding.bind_input("attention_mask", &attention_mask)?;

        // Bind past_key_values for each layer
        let cache_to_bind = if past_seq_len == 0 {
            &kv_cache
        } else {
            past_kv
        };
        for (layer_idx, (key, value)) in cache_to_bind.iter().enumerate() {
            let key_name = format!("past_key_values.{}.key", layer_idx);
            let value_name = format!("past_key_values.{}.value", layer_idx);

            binding.bind_input(&key_name, key)?;
            binding.bind_input(&value_name, value)?;
        }

        // Bind outputs to device memory (shape unknown, use bind_output_to_device)
        let mem_info = MemoryInfo::default(); // CPU memory
        binding.bind_output_to_device("logits", &mem_info)?;
        for layer_idx in 0..num_layers {
            let key_name = format!("present.{}.key", layer_idx);
            let value_name = format!("present.{}.value", layer_idx);

            binding.bind_output_to_device(&key_name, &mem_info)?;
            binding.bind_output_to_device(&value_name, &mem_info)?;
        }

        // Run inference (correct API: run_binding returns SessionOutputs)
        let mut outputs = self.session.run_binding(&binding)?;

        // Extract logits
        let logits = Self::extract_logits_static(&outputs, input_tokens.len())?;

        // Extract new KV cache by consuming outputs to get owned DynValues
        let mut new_cache = Vec::new();
        for layer_idx in 0..num_layers {
            let key_name = format!("present.{}.key", layer_idx);
            let value_name = format!("present.{}.value", layer_idx);

            // Get owned DynValue by removing from outputs
            let key_output = outputs
                .remove(&key_name)
                .ok_or_else(|| anyhow::anyhow!("Missing output: {}", key_name))?;
            let value_output = outputs
                .remove(&value_name)
                .ok_or_else(|| anyhow::anyhow!("Missing output: {}", value_name))?;

            new_cache.push((key_output, value_output));
        }

        Ok((logits, new_cache))
    }

    /// Prepare input tensor for ONNX Runtime
    fn prepare_input(&self, tokens: &[u32]) -> Result<DynValue> {
        debug!("Preparing input tensor: {} tokens", tokens.len());

        // Convert u32 tokens to i64 (ONNX typically expects int64)
        let input_data: Vec<i64> = tokens.iter().map(|&t| t as i64).collect();

        // Create tensor with shape [batch_size=1, seq_len]
        let array = ndarray::Array2::from_shape_vec((1, tokens.len()), input_data)
            .context("Failed to create ndarray for input")?;

        // Convert to ort::Value (ndarray feature enabled)
        // Use into_dyn() to erase the specific type to DynValueTypeMarker
        let value = Value::from_array(array).context("Failed to create ONNX Value from array")?;
        Ok(value.into_dyn())
    }

    /// Prepare position_ids tensor for ONNX Runtime
    fn prepare_position_ids(&self, seq_len: usize, past_seq_len: usize) -> Result<DynValue> {
        debug!(
            "Preparing position_ids: seq_len={}, past_seq_len={}",
            seq_len, past_seq_len
        );

        // Position IDs start from past_seq_len and go to past_seq_len + seq_len
        // For first step with seq_len=5: [0, 1, 2, 3, 4]
        // For second step with seq_len=1, past_seq_len=5: [5]
        let position_data: Vec<i64> = (past_seq_len..past_seq_len + seq_len)
            .map(|i| i as i64)
            .collect();

        // Create tensor with shape [batch_size=1, seq_len]
        let array = ndarray::Array2::from_shape_vec((1, seq_len), position_data)
            .context("Failed to create ndarray for position_ids")?;

        // Convert to ort::Value
        let value = Value::from_array(array)
            .context("Failed to create ONNX Value from position_ids array")?;
        Ok(value.into_dyn())
    }

    /// Prepare attention_mask tensor for ONNX Runtime
    fn prepare_attention_mask(&self, seq_len: usize, past_seq_len: usize) -> Result<DynValue> {
        debug!(
            "Preparing attention_mask: seq_len={}, past_seq_len={}",
            seq_len, past_seq_len
        );

        // Attention mask is all 1s for the total sequence length
        // Shape: [batch_size=1, total_seq_len]
        let total_seq_len = past_seq_len + seq_len;
        let mask_data: Vec<i64> = vec![1; total_seq_len];

        // Create tensor with shape [batch_size=1, total_seq_len]
        let array = ndarray::Array2::from_shape_vec((1, total_seq_len), mask_data)
            .context("Failed to create ndarray for attention_mask")?;

        // Convert to ort::Value
        let value = Value::from_array(array)
            .context("Failed to create ONNX Value from attention_mask array")?;
        Ok(value.into_dyn())
    }

    /// Extract logits from ONNX session output (static to avoid borrowing issues)
    fn extract_logits_static(outputs: &SessionOutputs, seq_len: usize) -> Result<Vec<f32>> {
        debug!("Extracting logits from output");

        // Get the first output by name (typically "logits" or similar)
        // Try common output names first
        let output_tensor = outputs
            .get("logits")
            .or_else(|| outputs.get("output"))
            .or_else(|| outputs.get("last_hidden_state"))
            .ok_or_else(|| anyhow::anyhow!("No output tensor found with expected names"))?;

        // Extract tensor data as f32
        // try_extract_tensor returns Result<(shape, data_slice)>
        let (shape, data) = output_tensor
            .try_extract_tensor::<f32>()
            .context("Failed to extract f32 tensor from output")?;

        debug!("Output tensor shape: {:?}", shape);

        // Shape is typically [batch_size, seq_len, vocab_size]
        if shape.len() != 3 {
            bail!("Expected 3D output tensor, got shape: {:?}", shape);
        }

        let vocab_size = shape[2] as usize;
        let last_token_offset = (seq_len - 1) * vocab_size;

        // Extract the last token's logits
        let logits: Vec<f32> = data
            .iter()
            .skip(last_token_offset)
            .take(vocab_size)
            .copied()
            .collect();

        debug!("Extracted {} logits for last token", logits.len());
        Ok(logits)
    }

    /// Sample next token from logits (greedy sampling) - static to avoid borrowing issues
    #[allow(dead_code)]
    fn sample_token_static(logits: &[f32]) -> Result<u32> {
        Self::sample_token_with_params(logits, &[], 0.7, 0.9, 1.1)
    }

    /// Sample token with temperature, top-p, and repetition penalty
    fn sample_token_with_params(
        logits: &[f32],
        previous_tokens: &[u32],
        temperature: f32,
        top_p: f32,
        repetition_penalty: f32,
    ) -> Result<u32> {
        if logits.is_empty() {
            bail!("Cannot sample from empty logits");
        }

        let mut scores = logits.to_vec();

        // Apply repetition penalty
        if repetition_penalty != 1.0 && !previous_tokens.is_empty() {
            for &token_id in previous_tokens {
                if (token_id as usize) < scores.len() {
                    let score = scores[token_id as usize];
                    // If score > 0, divide by penalty; if score < 0, multiply by penalty
                    scores[token_id as usize] = if score > 0.0 {
                        score / repetition_penalty
                    } else {
                        score * repetition_penalty
                    };
                }
            }
        }

        // Apply temperature
        if temperature != 1.0 {
            for score in &mut scores {
                *score /= temperature;
            }
        }

        // Convert logits to probabilities using softmax
        let max_score = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exp_scores: Vec<f32> = scores.iter().map(|&s| (s - max_score).exp()).collect();
        let sum_exp: f32 = exp_scores.iter().sum();
        let probs: Vec<f32> = exp_scores.iter().map(|&e| e / sum_exp).collect();

        // Create sorted indices by probability (descending)
        let mut indexed_probs: Vec<(usize, f32)> = probs.iter().cloned().enumerate().collect();
        indexed_probs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Top-p (nucleus) sampling
        let mut cumulative_prob = 0.0;
        let mut top_p_indices = Vec::new();

        for &(idx, prob) in &indexed_probs {
            cumulative_prob += prob;
            top_p_indices.push((idx, prob));
            if cumulative_prob >= top_p {
                break;
            }
        }

        // Renormalize probabilities for selected tokens
        let selected_prob_sum: f32 = top_p_indices.iter().map(|(_, p)| p).sum();
        if selected_prob_sum <= 0.0 {
            // Fallback to greedy if something went wrong
            if let Some(&(max_idx, _)) = indexed_probs.first() {
                return Ok(max_idx as u32);
            }
            bail!("No valid tokens to sample");
        }

        // Sample from the top-p distribution
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut rand_val: f32 = rng.gen::<f32>() * selected_prob_sum;

        for &(idx, prob) in &top_p_indices {
            rand_val -= prob;
            if rand_val <= 0.0 {
                debug!(
                    "Sampled token {} (prob: {:.4}, temp: {:.1}, top_p: {:.1})",
                    idx, prob, temperature, top_p
                );
                return Ok(idx as u32);
            }
        }

        // Fallback (should not reach here)
        if let Some(&(idx, _)) = top_p_indices.last() {
            Ok(idx as u32)
        } else {
            bail!("Failed to sample token")
        }
    }

    /// Get EOS token ID from tokenizer
    fn get_eos_token_id(&self) -> u32 {
        // Try to get from tokenizer's special tokens
        // For Qwen models, EOS is typically 151643
        // Fallback to common value if not available
        let vocab = self.tokenizer.get_vocab(true);

        vocab
            .get("<|endoftext|>")
            .or_else(|| vocab.get("<|im_end|>"))
            .or_else(|| vocab.get("</s>"))
            .copied()
            .unwrap_or(151643)
    }
}

// Implement TextGeneration trait
impl TextGeneration for LoadedOnnxModel {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        self.generate_autoregressive(input_ids, max_new_tokens)
    }

    fn generate_stream(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        token_callback: crate::models::TokenCallback,
    ) -> Result<Vec<u32>> {
        self.generate_autoregressive_with_callback(input_ids, max_new_tokens, Some(token_callback))
    }

    fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;
        Ok(encoding.get_ids().to_vec())
    }

    fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
        self.tokenizer
            .decode(tokens, true)
            .map_err(|e| anyhow::anyhow!("Decode failed: {}", e))
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl std::fmt::Debug for LoadedOnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedOnnxModel")
            .field("model_name", &self.model_name)
            .field("model_size", &self.model_size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::loaders::onnx_config::ExecutionProvider;

    #[test]
    fn test_execution_providers_default() {
        let providers = ExecutionProvider::default_for_platform();
        assert!(!providers.is_empty());

        #[cfg(target_os = "macos")]
        {
            assert_eq!(providers[0], ExecutionProvider::CoreML);
            assert_eq!(providers[1], ExecutionProvider::CPU);
        }
    }
}
