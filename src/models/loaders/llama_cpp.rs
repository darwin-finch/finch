//! In-process GGUF inference. Model identity and selection remain at the composition root.

use anyhow::{anyhow, bail, Context, Result};
use finch_memory::EmbeddingEngine;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use once_cell::sync::OnceCell;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use super::super::generator_new::{TextGeneration, TokenCallback};

// llama.cpp permits one initialized backend per process. Keep it alive longer
// than every model and context, including models loaded for memory and chat.
static BACKEND: OnceCell<LlamaBackend> = OnceCell::new();

fn backend() -> Result<&'static LlamaBackend> {
    BACKEND.get_or_try_init(|| LlamaBackend::init().context("initialize llama.cpp backend"))
}

fn load_model(path: &Path, allow_gpu_offload: bool) -> Result<Arc<LlamaModel>> {
    if !path.is_file() {
        bail!("GGUF model file does not exist: {}", path.display());
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("gguf") {
        bail!(
            "local llama.cpp model must be a .gguf file: {}",
            path.display()
        );
    }
    let backend = backend()?;
    let params = LlamaModelParams::default();
    #[cfg(not(target_os = "macos"))]
    let _ = allow_gpu_offload; // CPU-only build has no offload backend.
    #[cfg(target_os = "macos")]
    let params = if allow_gpu_offload && backend.supports_gpu_offload() {
        params.with_n_gpu_layers(1000)
    } else {
        params
    };
    let model = LlamaModel::load_from_file(backend, path, &params)
        .with_context(|| format!("load GGUF model from {}", path.display()))?;
    Ok(Arc::new(model))
}

fn context_params(embeddings: bool) -> LlamaContextParams {
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(2048))
        .with_embeddings(embeddings)
}

fn display_name(path: &Path, configured_family: Option<&str>) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("GGUF model filename must be UTF-8"))?;
    Ok(match configured_family {
        Some(family) => format!("{family} ({stem})"),
        None => stem.to_string(),
    })
}

/// GGUF text-generation backend selected explicitly by the local model loader.
pub(in crate::models) struct LlamaCppGenerator {
    model: Arc<LlamaModel>,
    name: String,
}

impl LlamaCppGenerator {
    /// Load a local chat GGUF under the configured family and offload policy.
    pub(in crate::models) fn load_with_offload(
        path: &Path,
        allow_gpu_offload: bool,
        configured_family: Option<&str>,
    ) -> Result<Self> {
        let name = display_name(path, configured_family)?;
        let model = load_model(path, allow_gpu_offload)?;
        Ok(Self { model, name })
    }

    fn token_bytes(&self, token: LlamaToken) -> Result<Vec<u8>> {
        match self.model.token_to_piece_bytes(token, 32, false, None) {
            Ok(piece) => Ok(piece),
            Err(llama_cpp_2::TokenToStringError::InsufficientBufferSpace(size)) => self
                .model
                .token_to_piece_bytes(token, usize::try_from(-size)?, false, None)
                .context("decode long GGUF token"),
            Err(error) => Err(error).context("decode GGUF token"),
        }
    }

    fn generate_inner(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        mut callback: Option<TokenCallback>,
    ) -> Result<Vec<u32>> {
        if input_ids.is_empty() {
            bail!("GGUF generation requires at least one prompt token");
        }
        let backend = backend()?;
        let mut context = self
            .model
            .new_context(backend, context_params(false))
            .context("create llama.cpp generation context")?;
        let capacity = usize::try_from(context.n_ctx()).context("GGUF context size overflow")?;
        if input_ids.len().saturating_add(max_new_tokens) > capacity {
            bail!(
                "GGUF prompt ({}) plus requested output ({}) exceeds context ({capacity})",
                input_ids.len(),
                max_new_tokens
            );
        }
        if max_new_tokens == 0 {
            return Ok(Vec::new());
        }
        let prompt = input_ids
            .iter()
            .map(|id| {
                i32::try_from(*id)
                    .map(LlamaToken)
                    .context("GGUF token ID overflow")
            })
            .collect::<Result<Vec<_>>>()?;
        let mut batch = LlamaBatch::new(capacity, 1);
        // Only the final prompt token needs logits for next-token sampling.
        for (index, token) in prompt.iter().copied().enumerate() {
            batch.add(token, index as i32, &[0], index + 1 == prompt.len())?;
        }
        context.decode(&mut batch).context("decode GGUF prompt")?;
        let mut sampler = LlamaSampler::greedy();
        let mut output = Vec::with_capacity(max_new_tokens);
        let mut pending_utf8 = Vec::new();
        for step in 0..max_new_tokens {
            let token = sampler.sample(&context, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) {
                break;
            }
            let id = u32::try_from(token.0).context("negative GGUF token ID")?;
            output.push(id);
            if let Some(callback) = callback.as_mut() {
                pending_utf8.extend(self.token_bytes(token)?);
                match std::str::from_utf8(&pending_utf8) {
                    Ok(chunk) => {
                        callback(id, chunk);
                        pending_utf8.clear();
                    }
                    Err(error) if error.error_len().is_none() => {} // split UTF-8 sequence
                    Err(error) => {
                        return Err(error).context("GGUF token stream contained invalid UTF-8")
                    }
                }
            }
            if step + 1 == max_new_tokens {
                break;
            }
            batch.clear();
            batch.add(token, (input_ids.len() + step) as i32, &[0], true)?;
            context
                .decode(&mut batch)
                .context("decode generated GGUF token")?;
        }
        if !pending_utf8.is_empty() {
            bail!("GGUF token stream ended in an incomplete UTF-8 sequence");
        }
        Ok(output)
    }
}

impl TextGeneration for LlamaCppGenerator {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        self.generate_inner(input_ids, max_new_tokens, None)
    }

    fn generate_stream(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        callback: TokenCallback,
    ) -> Result<Vec<u32>> {
        self.generate_inner(input_ids, max_new_tokens, Some(callback))
    }

    fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
        self.model
            .str_to_token(text, AddBos::Always)
            .context("tokenize GGUF prompt")?
            .into_iter()
            .map(|token| u32::try_from(token.0).context("negative GGUF token ID"))
            .collect()
    }

    fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
        let mut bytes = Vec::new();
        for id in tokens {
            let token = LlamaToken(i32::try_from(*id).context("GGUF token ID overflow")?);
            bytes.extend_from_slice(&self.token_bytes(token)?);
        }
        String::from_utf8(bytes).context("GGUF generated invalid UTF-8")
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Explicit GGUF embedding engine for memory injection. Callers own index identity/rebuild.
pub struct LlamaCppEmbeddingEngine {
    model: Arc<LlamaModel>,
    dimension: usize,
}

impl LlamaCppEmbeddingEngine {
    /// Load an embedding-capable GGUF; a pooling mode is required by `embed`.
    pub fn load(path: &Path) -> Result<Self> {
        let mut engine = Self {
            model: load_model(path, true)?,
            dimension: 0,
        };
        // The pooled output width can differ from the model's hidden width.
        // Probe the actual embedding port so callers never index a wrong size.
        engine.dimension = engine.embed("Finch embedding dimension probe")?.len();
        if engine.dimension < 8 {
            bail!(
                "GGUF output width {} is not a vector embedding; a reranker/classifier cannot back MemTree",
                engine.dimension
            );
        }
        Ok(engine)
    }
}

impl EmbeddingEngine for LlamaCppEmbeddingEngine {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self
            .model
            .str_to_token(text, AddBos::Always)
            .context("tokenize GGUF embedding input")?;
        if tokens.is_empty() {
            bail!("GGUF embedding input produced no tokens");
        }
        let mut context = self
            .model
            .new_context(backend()?, context_params(true))
            .context("create llama.cpp embedding context")?;
        let capacity = usize::try_from(context.n_ctx()).context("GGUF context size overflow")?;
        if tokens.len() > capacity {
            bail!("GGUF embedding input exceeds context ({capacity} tokens)");
        }
        let mut batch = LlamaBatch::new(capacity, 1);
        batch
            .add_sequence(&tokens, 0, false)
            .context("batch GGUF embedding input")?;
        context
            .decode(&mut batch)
            .context("decode GGUF embedding input")?;
        let embedding = context
            .embeddings_seq_ith(0)
            .context("GGUF model has no pooled sequence embedding")?;
        let magnitude = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if !magnitude.is_finite() || magnitude <= f32::EPSILON {
            return Err(anyhow!(
                "GGUF model returned a zero or non-finite embedding"
            ));
        }
        Ok(embedding.iter().map(|value| value / magnitude).collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_gguf_fails_before_native_initialization() {
        let error =
            LlamaCppGenerator::load_with_offload(Path::new("/nonexistent/finch.gguf"), true, None)
                .err()
                .expect("missing GGUF must fail");
        assert!(error.to_string().contains("does not exist"), "{error:#}");
    }

    #[test]
    fn test_non_gguf_fails_before_native_initialization() {
        let error = LlamaCppEmbeddingEngine::load(Path::new(file!()))
            .err()
            .expect("non-GGUF must fail");
        assert!(error.to_string().contains(".gguf"), "{error:#}");
    }

    #[test]
    fn test_configured_llm_family_survives_generic_gguf_filename() {
        let name =
            display_name(Path::new("/private/model.gguf"), Some("Gemma 2")).expect("display name");
        assert_eq!(name, "Gemma 2 (model)");
        assert_eq!(
            crate::models::AdapterRegistry::get_adapter(&name).family_name(),
            "Llama",
            "Gemma must use its configured family adapter, not the generic filename"
        );
        assert!(
            !name.contains("/private"),
            "model name must not leak its path"
        );
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_CHAT pointing to a local chat GGUF"]
    fn test_real_gguf_chat_generates_tokens() {
        let path = std::env::var("FINCH_TEST_GGUF_CHAT").expect("set FINCH_TEST_GGUF_CHAT");
        let mut generator = LlamaCppGenerator::load_with_offload(Path::new(&path), true, None)
            .expect("load GGUF chat");
        assert!(
            !generator.name().contains('/'),
            "public model identity must not reveal its local path"
        );
        let tokens = generator.tokenize("Hello, my name is").expect("tokenize");
        let streamed = Arc::new(std::sync::Mutex::new(String::new()));
        let received = Arc::clone(&streamed);
        let output = generator
            .generate_stream(
                &tokens,
                8,
                Box::new(move |_, piece| {
                    received.lock().expect("lock stream").push_str(piece);
                }),
            )
            .expect("generate with callbacks");
        assert!(!output.is_empty(), "GGUF chat must emit at least one token");
        let text = generator.decode_tokens(&output).expect("decode");
        assert!(
            !text.trim().is_empty(),
            "GGUF chat output must contain text"
        );
        assert_eq!(
            *streamed.lock().expect("lock stream"),
            text,
            "streamed chunks must reconstruct the decoded generation"
        );
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_CHAT pointing to a local chat GGUF"]
    fn test_real_gguf_explicit_cpu_generates() {
        let path = std::env::var("FINCH_TEST_GGUF_CHAT").expect("set FINCH_TEST_GGUF_CHAT");
        let mut generator =
            LlamaCppGenerator::load_with_offload(Path::new(&path), false, Some("Qwen 2.5"))
                .expect("load GGUF without GPU offload");
        let prompt = generator.tokenize("Hello").expect("tokenize");
        let output = generator.generate(&prompt, 4).expect("CPU GGUF generation");
        assert!(!output.is_empty(), "CPU GGUF generation must emit tokens");
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_EMBED pointing to a local embedding GGUF"]
    fn test_real_gguf_embeddings_are_distinct_and_normalized() {
        let path = std::env::var("FINCH_TEST_GGUF_EMBED").expect("set FINCH_TEST_GGUF_EMBED");
        let engine = LlamaCppEmbeddingEngine::load(Path::new(&path)).expect("load embedding GGUF");
        let first = engine.embed("a small red bird").expect("embed first");
        let second = engine
            .embed("a database transaction")
            .expect("embed second");
        assert_eq!(
            first.len(),
            engine.dimension(),
            "embedding width must match port"
        );
        assert_eq!(second.len(), first.len(), "embedding widths must be stable");
        assert!(
            first.iter().zip(&second).any(|(a, b)| (a - b).abs() > 1e-4),
            "distinct inputs must not produce identical embeddings"
        );
        let norm = first.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "embedding must be normalized: {norm}"
        );
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_RERANK pointing to a local ranking GGUF"]
    fn test_reranker_cannot_be_used_as_memory_embedder() {
        let path = std::env::var("FINCH_TEST_GGUF_RERANK").expect("set FINCH_TEST_GGUF_RERANK");
        let error = LlamaCppEmbeddingEngine::load(Path::new(&path))
            .err()
            .expect("ranking model must not be accepted as an embedding engine");
        assert!(
            error.to_string().contains("not a vector embedding")
                || error.to_string().contains("no pooled sequence embedding"),
            "reranker must fail with a model-capability explanation: {error:#}"
        );
    }
}
