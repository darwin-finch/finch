//! In-process GGUF inference. Model identity and selection remain at the composition root.

use anyhow::{anyhow, bail, Context, Result};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::{LlamaStateSeqFlags, SeqState};
use once_cell::sync::OnceCell;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use super::super::generator_new::{TextGeneration, TokenCallback};

// llama.cpp permits one initialized backend per process. Keep it alive longer
// than every model and context used for local chat.
static BACKEND: OnceCell<LlamaBackend> = OnceCell::new();

fn backend() -> Result<&'static LlamaBackend> {
    BACKEND.get_or_try_init(|| LlamaBackend::init().context("initialize llama.cpp backend"))
}

fn prompt_contains_explicit_bos(text: &str) -> bool {
    // Each of these is a chat-template adapter's literal, human-readable BOS
    // marker: Llama 3's `<|begin_of_text|>`, Llama 2/Mistral's `<s>`, and
    // Gemma's `<bos>`. llama.cpp's tokenizer parses special-token text in the
    // prompt (parse_special=true in `str_to_token`) regardless of the AddBos
    // flag, so when the adapter already wrote one of these into the prompt,
    // AddBos must be Never or the model sees a duplicate BOS token.
    text.starts_with("<|begin_of_text|>") || text.starts_with("<s>") || text.starts_with("<bos>")
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

/// llama.cpp's own generic default, used only when a model's trained
/// context length cannot be determined (`n_ctx_train()` returning `0`,
/// which the C API otherwise never does for a valid GGUF).
const FALLBACK_CONTEXT_TOKENS: u32 = 2048;

/// Ceiling on the context window this loader will ever request from
/// llama.cpp, independent of how large a model's own trained context is.
///
/// llama.cpp allocates the KV cache for the *entire* requested `n_ctx` up
/// front, and that allocation scales linearly with context length
/// regardless of how much of it a given conversation actually uses. Some
/// GGUFs (long-context Llama/Qwen variants in particular) are trained for
/// context lengths in the tens or low hundreds of thousands of tokens;
/// unconditionally requesting a model's full trained length risks
/// exhausting memory on the CPU-only, non-GPU-offload hardware this
/// in-process backend targets (local routing and provider parity remain
/// experimental: Issues #74, #98). 8192 is chosen because it exactly
/// matches Gemma 2 9B's trained length -- the motivating case for this fix
/// -- and comfortably covers ordinary coding-assistant turns without the
/// multi-gigabyte KV cache a 32K+ context would allocate on CPU. Revisit
/// this once a config knob exists, or once the vendored
/// `LlamaModelParams::fit_params` VRAM-fitting API is wired up for a
/// memory-aware CPU path.
const MAX_LOCAL_CONTEXT_TOKENS: u32 = 8192;

/// Resolve the context window to request from a model's own trained
/// length, capped at `MAX_LOCAL_CONTEXT_TOKENS` and falling back to
/// llama.cpp's generic default only if the trained length is unknown.
///
/// A pure function (no model I/O) so its clamping behaviour is unit
/// testable without loading a real GGUF.
fn resolve_context_tokens(n_ctx_train: u32) -> u32 {
    if n_ctx_train == 0 {
        FALLBACK_CONTEXT_TOKENS
    } else {
        n_ctx_train.min(MAX_LOCAL_CONTEXT_TOKENS)
    }
}

/// Context size to request for `model`, read from the GGUF's own trained
/// context length (`llama_n_ctx_train`, exposed as `LlamaModel::n_ctx_train`)
/// rather than a hardcoded value that ignores what the loaded model was
/// actually trained for. Gemma 2 9B, for example, is trained for 8192
/// tokens; a fixed 2048 silently discarded three quarters of its real
/// usable context.
fn context_params(model: &LlamaModel) -> LlamaContextParams {
    let n_ctx = resolve_context_tokens(model.n_ctx_train());
    LlamaContextParams::default().with_n_ctx(NonZeroU32::new(n_ctx))
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
    prompt_cache: Option<PromptCache>,
    #[cfg(test)]
    last_cached_prompt_tokens: usize,
}

struct PromptCache {
    tokens: Vec<u32>,
    state: SeqState,
}

fn reusable_prompt_tokens(cached: &[u32], requested: &[u32]) -> usize {
    if cached.len() < requested.len() && requested.starts_with(cached) {
        cached.len()
    } else {
        0
    }
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
        Ok(Self {
            model,
            name,
            prompt_cache: None,
            #[cfg(test)]
            last_cached_prompt_tokens: 0,
        })
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
            .new_context(backend, context_params(&self.model))
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
        let cached_tokens = self
            .prompt_cache
            .as_ref()
            .map_or(0, |cache| reusable_prompt_tokens(&cache.tokens, input_ids));
        #[cfg(test)]
        {
            self.last_cached_prompt_tokens = cached_tokens;
        }
        if cached_tokens > 0 {
            let cache = self
                .prompt_cache
                .as_ref()
                .expect("cache length came from cache");
            context
                .state_seq_set(&cache.state, 0)
                .context("restore cached GGUF prompt state")?;
        }
        let mut batch = LlamaBatch::new(capacity, 1);
        // A cache hit always leaves a non-empty suffix. Only its final token
        // needs logits for next-token sampling.
        for (index, token) in prompt.iter().copied().enumerate().skip(cached_tokens) {
            batch.add(token, index as i32, &[0], index + 1 == prompt.len())?;
        }
        tracing::debug!(
            model = %self.name,
            prompt_tokens = input_ids.len(),
            cached_prompt_tokens = cached_tokens,
            evaluated_prompt_tokens = input_ids.len() - cached_tokens,
            "evaluating llama.cpp prompt"
        );
        context.decode(&mut batch).context("decode GGUF prompt")?;
        let state = context
            .state_seq_get(0, LlamaStateSeqFlags::empty())
            .context("snapshot GGUF prompt state")?;
        self.prompt_cache = Some(PromptCache {
            tokens: input_ids.to_vec(),
            state,
        });
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
        let add_bos = if prompt_contains_explicit_bos(text) {
            AddBos::Never
        } else {
            AddBos::Always
        };
        self.model
            .str_to_token(text, add_bos)
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

    fn context_length(&self) -> u32 {
        resolve_context_tokens(self.model.n_ctx_train())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
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

    /// Regression for the hardcoded `n_ctx=2048`: a model trained for a
    /// longer context (Gemma 2 9B's real 8192) must get that length, not
    /// llama.cpp's generic default, as long as it fits under the memory
    /// ceiling.
    #[test]
    fn test_resolve_context_tokens_uses_model_trained_length_when_under_ceiling() {
        assert_eq!(
            resolve_context_tokens(8192),
            8192,
            "Gemma 2 9B's real trained context (8192) must be used verbatim, not \
             discarded down to llama.cpp's generic 2048 default"
        );
        assert_eq!(
            resolve_context_tokens(4096),
            4096,
            "a model trained for 4096 must not be clamped down to the old 2048 default"
        );
    }

    /// Some GGUFs (long-context Llama/Qwen variants) train for far more
    /// tokens than a CPU-only KV cache should unconditionally allocate.
    #[test]
    fn test_resolve_context_tokens_caps_very_long_trained_models_at_the_memory_ceiling() {
        assert_eq!(
            resolve_context_tokens(131_072),
            MAX_LOCAL_CONTEXT_TOKENS,
            "a 128K-trained model must be capped at the memory ceiling, not requested in full"
        );
    }

    /// `llama_n_ctx_train` should never return 0 for a valid GGUF, but the
    /// resolver must not construct a zero-sized context if it somehow does.
    #[test]
    fn test_resolve_context_tokens_falls_back_when_trained_length_is_unknown() {
        assert_eq!(
            resolve_context_tokens(0),
            FALLBACK_CONTEXT_TOKENS,
            "an unknown (zero) trained length must fall back to llama.cpp's generic default, \
             not construct a zero-sized context"
        );
    }

    #[test]
    fn prompt_cache_reuses_only_an_exact_strict_prefix() {
        assert_eq!(reusable_prompt_tokens(&[1, 2, 3], &[1, 2, 3, 4, 5]), 3);
        assert_eq!(reusable_prompt_tokens(&[1, 2, 3], &[1, 2, 9, 4]), 0);
        assert_eq!(reusable_prompt_tokens(&[1, 2, 3], &[1, 2, 3]), 0);
        assert_eq!(reusable_prompt_tokens(&[1, 2, 3], &[1, 2]), 0);
    }

    #[test]
    fn test_non_gguf_fails_before_native_initialization() {
        let error = LlamaCppGenerator::load_with_offload(Path::new(file!()), true, None)
            .err()
            .expect("non-GGUF must fail");
        assert!(error.to_string().contains(".gguf"), "{error:#}");
    }

    #[test]
    fn explicit_chat_template_bos_disables_tokenizer_bos_insertion() {
        assert!(prompt_contains_explicit_bos(
            "<|begin_of_text|><|start_header_id|>system"
        ));
        assert!(prompt_contains_explicit_bos("<s>[INST] hello [/INST]"));
        assert!(prompt_contains_explicit_bos("<bos><start_of_turn>user"));
        assert!(!prompt_contains_explicit_bos("<|im_start|>system"));
    }

    #[test]
    fn test_configured_llm_family_survives_generic_gguf_filename() {
        let name =
            display_name(Path::new("/private/model.gguf"), Some("Gemma 2")).expect("display name");
        assert_eq!(name, "Gemma 2 (model)");
        assert_eq!(
            crate::models::AdapterRegistry::get_adapter(&name).family_name(),
            "Gemma",
            "Gemma must use its configured family adapter, not the generic filename"
        );
        assert!(
            !name.contains("/private"),
            "model name must not leak its path"
        );
    }

    /// Production-boundary regression for the hardcoded `n_ctx=2048`: loads a
    /// real GGUF and checks the context this loader actually constructs
    /// reflects that model's own trained length (capped at
    /// `MAX_LOCAL_CONTEXT_TOKENS`), not llama.cpp's generic default -- the
    /// unit tests above cover the clamping arithmetic in isolation, but only
    /// this exercises `LlamaModel::n_ctx_train()` and `LlamaContextParams`
    /// construction against the real native binding.
    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_CHAT pointing to a local chat GGUF"]
    fn test_real_gguf_context_size_reflects_model_trained_length_not_hardcoded_default() {
        let path = std::env::var("FINCH_TEST_GGUF_CHAT").expect("set FINCH_TEST_GGUF_CHAT");
        let generator = LlamaCppGenerator::load_with_offload(Path::new(&path), true, None)
            .expect("load GGUF chat");
        let trained = generator.model.n_ctx_train();
        let expected = resolve_context_tokens(trained);
        assert_ne!(
            expected, 0,
            "resolve_context_tokens must never resolve to a zero-sized context; trained={trained}"
        );
        let params = context_params(&generator.model);
        assert_eq!(
            params.n_ctx(),
            NonZeroU32::new(expected),
            "LlamaContextParams must carry this model's real (trained, capped) context \
             length, not llama.cpp's generic 2048 default; trained={trained}, expected={expected}, \
             got={:?}",
            params.n_ctx()
        );
        assert_eq!(
            generator.context_length(),
            expected,
            "TextGeneration::context_length() must report the same resolved value used to \
             construct the generation context, so budget-aware callers see the truth"
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

        let mut continued = tokens.clone();
        continued.extend(output);
        continued.extend(
            generator
                .tokenize(" And then")
                .expect("tokenize continuation"),
        );
        generator
            .generate(&continued, 1)
            .expect("generate from cached prompt prefix");
        assert_eq!(
            generator.last_cached_prompt_tokens,
            tokens.len(),
            "a continuing conversation must restore the preceding prompt state"
        );
    }

    #[test]
    #[ignore = "requires FINCH_TEST_LLAMA_GGUF pointing to a Llama 3 GGUF"]
    fn test_real_llama_gguf_prompt_contains_one_bos_token() {
        let path = std::env::var("FINCH_TEST_LLAMA_GGUF").expect("set FINCH_TEST_LLAMA_GGUF");
        let generator =
            LlamaCppGenerator::load_with_offload(Path::new(&path), true, Some("Llama 3"))
                .expect("load Llama GGUF");
        let adapter = crate::models::LlamaAdapter;
        let prompt = crate::models::LocalModelAdapter::format_chat_prompt(
            &adapter,
            "You are helpful.",
            "Hello",
        );
        let tokens = generator.tokenize(&prompt).expect("tokenize Llama prompt");
        assert_eq!(
            tokens.iter().filter(|token| **token == 128000).count(),
            1,
            "the explicit Llama template BOS must not be duplicated"
        );
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GEMMA_GGUF pointing to a Gemma 2 GGUF"]
    fn test_real_gemma_gguf_prompt_contains_one_bos_token_and_generates() {
        let path = std::env::var("FINCH_TEST_GEMMA_GGUF").expect("set FINCH_TEST_GEMMA_GGUF");
        let mut generator =
            LlamaCppGenerator::load_with_offload(Path::new(&path), true, Some("Gemma 2"))
                .expect("load Gemma GGUF");
        let adapter = crate::models::GemmaAdapter;
        let prompt = crate::models::LocalModelAdapter::format_chat_prompt(
            &adapter,
            "You are helpful.",
            "Hello",
        );
        let tokens = generator.tokenize(&prompt).expect("tokenize Gemma prompt");
        assert_eq!(
            tokens.iter().filter(|token| **token == 2).count(),
            1,
            "Gemma's literal <bos> template marker must tokenize to exactly one real BOS (id 2), \
             not zero (misrouted to Llama's <|begin_of_text|>, which Gemma's vocab doesn't have) \
             and not two (AddBos::Always duplicating the literal <bos> already in the prompt)"
        );
        // This is the exact symptom this adapter fixes: a misformatted Gemma
        // prompt plausibly samples an end-of-generation token on the very
        // first greedy step, yielding an empty response with no error.
        let output = generator
            .generate(&tokens, 4)
            .expect("Gemma GGUF generation");
        assert!(
            !output.is_empty(),
            "a correctly formatted Gemma prompt must not generate zero tokens"
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
}
