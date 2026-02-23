// Candle Model Loader - Native Rust inference implementation
//
// Provides an alternative to ONNX Runtime using Candle (pure Rust ML framework)
// Feature parity with ONNX loader via TextGeneration trait

#[cfg(feature = "candle")]
use anyhow::{Context, Result};
#[cfg(feature = "candle")]
use std::path::Path;

#[cfg(feature = "candle")]
use candle_core::{Device, Tensor};
#[cfg(feature = "candle")]
use candle_nn::VarBuilder;
#[cfg(feature = "candle")]
use candle_transformers::models;

#[cfg(feature = "candle")]
use super::super::generator_new::TextGeneration;
#[cfg(feature = "candle")]
use super::super::unified_loader::{ModelFamily, ModelSize};
#[cfg(feature = "candle")]
use crate::config::ExecutionTarget;

/// Candle model loader
#[cfg(feature = "candle")]
pub struct CandleLoader;

#[cfg(feature = "candle")]
impl CandleLoader {
    /// Create new Candle loader
    pub fn new() -> Self {
        Self
    }

    /// Load model from path
    pub fn load(
        &self,
        model_path: &Path,
        family: ModelFamily,
        size: ModelSize,
        target: ExecutionTarget,
    ) -> Result<Box<dyn TextGeneration>> {
        let device = Self::get_device(target)?;

        match family {
            ModelFamily::Qwen2 => self.load_qwen(model_path, size, device),
            ModelFamily::Llama3 => self.load_llama(size),
            ModelFamily::Gemma2 => self.load_gemma(size),
            ModelFamily::Mistral => self.load_mistral(size),
            ModelFamily::Phi => self.load_phi(size),
            ModelFamily::DeepSeek => self.load_deepseek(size),
        }
    }

    /// Get Candle device from execution target
    fn get_device(target: ExecutionTarget) -> Result<Device> {
        match target {
            #[cfg(target_os = "macos")]
            ExecutionTarget::CoreML => {
                // CoreML is ONNX-specific; use Metal for Candle on macOS
                Device::new_metal(0).context("Failed to initialize Metal device")
            }

            #[cfg(feature = "cuda")]
            ExecutionTarget::Cuda => {
                Device::new_cuda(0).context("Failed to initialize CUDA device")
            }

            ExecutionTarget::Cpu | ExecutionTarget::Auto => Ok(Device::Cpu),
        }
    }

    /// Load Qwen 2.5 model (the only family currently implemented for Candle)
    fn load_qwen(
        &self,
        model_path: &Path,
        size: ModelSize,
        device: Device,
    ) -> Result<Box<dyn TextGeneration>> {
        tracing::info!("Loading Qwen {:?} model with Candle", size);

        let tokenizer_path = model_path.join("tokenizer.json");
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        let config_path = model_path.join("config.json");
        let config_str =
            std::fs::read_to_string(&config_path).context("Failed to read config.json")?;
        let config: models::qwen2::Config =
            serde_json::from_str(&config_str).context("Failed to parse config.json")?;

        let weights_path = model_path.join("model.safetensors");
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)
                .context("Failed to load model weights")?
        };

        let model = models::qwen2::Model::new(&config, vb).context("Failed to build Qwen model")?;

        Ok(Box::new(LoadedCandleModel {
            model: CandleModel::Qwen(model),
            tokenizer,
            device,
        }))
    }

    // --- Stubs for families not yet implemented ---

    fn load_llama(&self, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("Llama {:?} not yet implemented for Candle", size)
    }

    fn load_gemma(&self, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("Gemma {:?} not yet implemented for Candle", size)
    }

    fn load_mistral(&self, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("Mistral {:?} not yet implemented for Candle", size)
    }

    fn load_phi(&self, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("Phi {:?} not yet implemented for Candle", size)
    }

    fn load_deepseek(&self, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("DeepSeek {:?} not yet implemented for Candle", size)
    }
}

/// Enum for different Candle model types
#[cfg(feature = "candle")]
enum CandleModel {
    Qwen(models::qwen2::Model),
    // More families will be added here as they are implemented
}

/// Loaded Candle model implementing the TextGeneration trait
#[cfg(feature = "candle")]
pub struct LoadedCandleModel {
    model: CandleModel,
    tokenizer: tokenizers::Tokenizer,
    device: Device,
}

#[cfg(feature = "candle")]
impl LoadedCandleModel {
    /// Get model name
    pub fn model_name(&self) -> &str {
        match &self.model {
            CandleModel::Qwen(_) => "Qwen (Candle)",
        }
    }
}

#[cfg(feature = "candle")]
impl TextGeneration for LoadedCandleModel {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        use candle_core::IndexOp;

        // Qwen EOS tokens: <|endoftext|> = 151643, <|im_end|> = 151645
        let eos_id = self
            .tokenizer
            .token_to_id("<|endoftext|>")
            .or_else(|| self.tokenizer.token_to_id("<|im_end|>"))
            .unwrap_or(151643);

        let mut output_ids = input_ids.to_vec();

        // Clear internal KV cache from any previous call
        match &mut self.model {
            CandleModel::Qwen(model) => model.clear_kv_cache(),
        }

        // Initial forward pass with the full prompt (seqlen_offset = 0)
        let prompt_tensor = Tensor::new(input_ids, &self.device)
            .context("Failed to create prompt tensor")?
            .unsqueeze(0)?;

        let logits = match &mut self.model {
            CandleModel::Qwen(model) => model
                .forward(&prompt_tensor, 0, None)
                .context("Initial forward pass failed")?,
        };

        // Pick the next token from the last prompt position
        let last_logits = logits.i((0, logits.dim(1)? - 1))?;
        let first_token = last_logits.argmax(0)?.to_scalar::<u32>()?;

        if first_token == eos_id {
            return Ok(output_ids);
        }
        output_ids.push(first_token);

        // Autoregressive loop — one new token per step, KV cache accumulates internally
        let mut seqlen_offset = input_ids.len();
        let mut prev_token = first_token;

        for _ in 1..max_new_tokens {
            let token_tensor = Tensor::new(&[prev_token], &self.device)?.unsqueeze(0)?;

            let logits = match &mut self.model {
                CandleModel::Qwen(model) => model
                    .forward(&token_tensor, seqlen_offset, None)
                    .context("Forward pass failed")?,
            };
            seqlen_offset += 1;

            let step_logits = logits.i((0, 0))?;
            let next_token = step_logits.argmax(0)?.to_scalar::<u32>()?;

            if next_token == eos_id {
                break;
            }
            output_ids.push(next_token);
            prev_token = next_token;
        }

        Ok(output_ids)
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
        self.model_name()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// Placeholder when candle feature is disabled
#[cfg(not(feature = "candle"))]
pub struct CandleLoader;

#[cfg(not(feature = "candle"))]
impl CandleLoader {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(all(test, feature = "candle"))]
mod tests {
    use super::*;
    use crate::config::ExecutionTarget;
    use crate::models::unified_loader::{ModelFamily, ModelSize};

    // --- Regression: unimplemented families must return errors, not panic ---
    //
    // These verify that selecting a non-Qwen family with the Candle backend gives
    // a clear error at load time rather than silently doing nothing or panicking.

    fn loader_with_dummy_path() -> (CandleLoader, std::path::PathBuf) {
        (
            CandleLoader::new(),
            std::path::PathBuf::from("/nonexistent"),
        )
    }

    #[test]
    fn test_candle_llama_stub_returns_error() {
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::Llama3,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        let msg = result.err().expect("expected an error").to_string();
        assert!(
            msg.contains("not yet implemented"),
            "Expected stub error, got: {}",
            msg
        );
    }

    #[test]
    fn test_candle_gemma_stub_returns_error() {
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::Gemma2,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        assert!(result
            .err()
            .expect("expected an error")
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn test_candle_mistral_stub_returns_error() {
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::Mistral,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        assert!(result
            .err()
            .expect("expected an error")
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn test_candle_phi_stub_returns_error() {
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::Phi,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        assert!(result
            .err()
            .expect("expected an error")
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn test_candle_deepseek_stub_returns_error() {
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::DeepSeek,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        assert!(result
            .err()
            .expect("expected an error")
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn test_candle_qwen_missing_files_returns_error() {
        // Qwen IS implemented; loading from a nonexistent path must fail gracefully
        let (loader, path) = loader_with_dummy_path();
        let result = loader.load(
            &path,
            ModelFamily::Qwen2,
            ModelSize::Small,
            ExecutionTarget::Cpu,
        );
        assert!(result.is_err());
        // Should fail on tokenizer/config/weights load, not a stub error
        let msg = result.err().expect("expected an error").to_string();
        assert!(
            !msg.contains("not yet implemented"),
            "Qwen should attempt to load, not stub: {}",
            msg
        );
    }
}
