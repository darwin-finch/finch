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

/// Logical batch size (`n_batch`): the most tokens this loader will ever
/// submit to a single llama.cpp `decode()` call. llama.cpp does not chunk a
/// `decode()` call internally -- `GGML_ASSERT(n_tokens_all <=
/// cparams.n_batch)` in `llama-context.cpp` aborts the whole process (a C++
/// `abort()`, not a catchable Rust panic or `Result::Err`) if a caller ever
/// exceeds it. Issue #1292 traced a live production SIGABRT to exactly this
/// assertion: a fresh Brain's first turn (one recalled memory, ordinary
/// conversation, and the tool-definitions block) produced a 7991-token
/// prompt evaluated in one `decode()` call.
///
/// `generate_inner` below is the actual fix: it chunks any prompt longer
/// than `n_batch` into successive `decode()` calls, so this constant bounds
/// per-call memory and throughput rather than correctness -- prompts are
/// unbounded relative to any fixed value here. 2048 is llama.cpp's own
/// library default (`llama_context_default_params()`) and matches
/// `llama-server`'s default logical batch size; it is set explicitly here
/// (rather than left to the crate default) so the value has a documented,
/// intentional home instead of an implicit one a future crate upgrade could
/// silently change.
const LOCAL_N_BATCH: u32 = 2048;

/// Physical batch size (`n_ubatch`): the number of tokens llama.cpp actually
/// computes over in one forward pass. llama.cpp requires `n_ubatch <=
/// n_batch`; a single decode() call larger than `n_ubatch` is internally
/// split into sub-batches of at most this size for compute, independent of
/// the chunking `generate_inner` does at the `n_batch` level above. 512 is
/// llama.cpp's own library default and matches `llama-server`'s default,
/// keeping the per-forward-pass compute-buffer memory bounded even when
/// `LOCAL_N_BATCH` admits a much larger logical batch.
const LOCAL_N_UBATCH: u32 = 512;

/// Resolve `(n_batch, n_ubatch)` for a context of `n_ctx` tokens, capping
/// both at `n_ctx`. llama.cpp computes `cparams.n_batch =
/// min(n_ctx, params.n_batch)` itself for causal models, but an unusually
/// small trained context (below `LOCAL_N_UBATCH`) would otherwise leave
/// `n_ubatch > n_batch`, which llama.cpp does not permit. A pure function so
/// the clamping is unit testable without a real GGUF.
fn resolve_batch_sizes(n_ctx: u32) -> (u32, u32) {
    let n_batch = LOCAL_N_BATCH.min(n_ctx);
    let n_ubatch = LOCAL_N_UBATCH.min(n_batch);
    (n_batch, n_ubatch)
}

/// Context size to request for `model`, read from the GGUF's own trained
/// context length (`llama_n_ctx_train`, exposed as `LlamaModel::n_ctx_train`)
/// rather than a hardcoded value that ignores what the loaded model was
/// actually trained for. Gemma 2 9B, for example, is trained for 8192
/// tokens; a fixed 2048 silently discarded three quarters of its real
/// usable context.
fn context_params(model: &LlamaModel) -> LlamaContextParams {
    let n_ctx = resolve_context_tokens(model.n_ctx_train());
    let (n_batch, n_ubatch) = resolve_batch_sizes(n_ctx);
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_batch)
        .with_n_ubatch(n_ubatch)
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

/// One `decode()` call's worth of absolute prompt-token positions.
/// `end` is exclusive. The caller submits `prompt[start..end]` with each
/// token's llama.cpp `pos` equal to its absolute index, so splitting a
/// prompt into chunks never changes a token's position versus evaluating it
/// in one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodeChunk {
    start: usize,
    end: usize,
}

/// Splits the uncached suffix `cached_tokens..total_tokens` into successive
/// windows of at most `n_batch` tokens each, in order. llama.cpp requires a
/// single `decode()` call's token count to fit within `n_batch`
/// (`GGML_ASSERT(n_tokens_all <= cparams.n_batch)` in `llama-context.cpp`;
/// see `LOCAL_N_BATCH` for why exceeding it aborts the whole process). A
/// pure function so the chunk boundaries -- and their composition with the
/// prompt-cache skip-forward -- are unit testable without a real GGUF.
fn plan_prompt_decode_chunks(
    cached_tokens: usize,
    total_tokens: usize,
    n_batch: usize,
) -> Vec<DecodeChunk> {
    let n_batch = n_batch.max(1);
    (cached_tokens..total_tokens)
        .step_by(n_batch)
        .map(|start| DecodeChunk {
            start,
            end: (start + n_batch).min(total_tokens),
        })
        .collect()
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
        let n_batch = usize::try_from(context.n_batch()).context("GGUF n_batch overflow")?;
        let mut batch = LlamaBatch::new(n_batch.max(1), 1);
        // A cache hit always leaves a non-empty suffix (see
        // `reusable_prompt_tokens`), so `chunks` below is always non-empty.
        let chunks = plan_prompt_decode_chunks(cached_tokens, prompt.len(), n_batch);
        tracing::debug!(
            model = %self.name,
            prompt_tokens = input_ids.len(),
            cached_prompt_tokens = cached_tokens,
            evaluated_prompt_tokens = input_ids.len() - cached_tokens,
            n_batch,
            decode_chunks = chunks.len(),
            "evaluating llama.cpp prompt"
        );
        // llama.cpp requires a single decode() call's token count to fit
        // within n_batch (see `LOCAL_N_BATCH`), so a prompt longer than
        // n_batch is submitted across successive decode() calls. Only the
        // prompt's true final token ever needs logits for next-token
        // sampling, regardless of which chunk it lands in.
        for chunk in &chunks {
            batch.clear();
            for absolute_index in chunk.start..chunk.end {
                let wants_logits = absolute_index + 1 == prompt.len();
                batch.add(
                    prompt[absolute_index],
                    i32::try_from(absolute_index).context("GGUF prompt position overflow")?,
                    &[0],
                    wants_logits,
                )?;
            }
            context
                .decode(&mut batch)
                .context("decode GGUF prompt chunk")?;
        }
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

    /// Regression for issue #1292: with an ordinary (large) trained context,
    /// the batch sizes must be the deliberate constants, not silently
    /// whatever the crate's `LlamaContextParams::default()` carries.
    #[test]
    fn test_resolve_batch_sizes_uses_local_constants_under_context_ceiling() {
        assert_eq!(
            resolve_batch_sizes(8192),
            (LOCAL_N_BATCH, LOCAL_N_UBATCH),
            "an 8192 context must not clamp n_batch/n_ubatch below their configured constants"
        );
    }

    /// llama.cpp computes `cparams.n_batch = min(n_ctx, params.n_batch)`
    /// itself, but leaving `n_ubatch` unclamped for an unusually small
    /// trained context would request `n_ubatch > n_batch`, which llama.cpp
    /// does not permit.
    #[test]
    fn test_resolve_batch_sizes_caps_both_at_a_small_context() {
        assert_eq!(
            resolve_batch_sizes(256),
            (256, 256),
            "n_batch and n_ubatch must both be capped at a context (256) smaller than either \
             configured constant (n_batch={LOCAL_N_BATCH}, n_ubatch={LOCAL_N_UBATCH})"
        );
    }

    /// Core regression for issue #1292's chunking math: a prompt longer
    /// than `n_batch` must be split into successive windows of at most
    /// `n_batch` tokens, covering the uncached suffix exactly once with no
    /// gap or overlap.
    #[test]
    fn test_plan_prompt_decode_chunks_splits_long_prompt_into_n_batch_sized_windows() {
        let chunks = plan_prompt_decode_chunks(0, 1030, 512);
        assert_eq!(
            chunks,
            vec![
                DecodeChunk { start: 0, end: 512 },
                DecodeChunk {
                    start: 512,
                    end: 1024
                },
                DecodeChunk {
                    start: 1024,
                    end: 1030
                },
            ],
            "a 1030-token prompt over n_batch=512 must produce exactly three chunks of \
             512, 512, and 6 tokens covering [0, 1030) with no gap or overlap: got {chunks:?}"
        );
        for chunk in &chunks {
            assert!(
                chunk.end - chunk.start <= 512,
                "chunk {chunk:?} exceeds n_batch=512, which is exactly the native \
                 GGML_ASSERT(n_tokens_all <= cparams.n_batch) this planner exists to satisfy"
            );
        }
    }

    /// The prompt-cache skip-forward (`reusable_prompt_tokens`) must compose
    /// with chunking: only the uncached suffix is chunked, and the first
    /// chunk starts at `cached_tokens`, not 0.
    #[test]
    fn test_plan_prompt_decode_chunks_composes_with_cache_hit_skip_forward() {
        let chunks = plan_prompt_decode_chunks(300, 1300, 512);
        assert_eq!(
            chunks,
            vec![
                DecodeChunk {
                    start: 300,
                    end: 812
                },
                DecodeChunk {
                    start: 812,
                    end: 1300
                },
            ],
            "chunking a cache hit (300 cached of 1300 total) must start at the cached \
             boundary, not re-evaluate already-cached tokens from 0: got {chunks:?}"
        );
    }

    /// A prompt no longer than `n_batch` must still produce exactly one
    /// chunk (the pre-fix, single-decode() behaviour for a small prompt).
    #[test]
    fn test_plan_prompt_decode_chunks_prompt_under_n_batch_is_a_single_chunk() {
        assert_eq!(
            plan_prompt_decode_chunks(0, 10, 512),
            vec![DecodeChunk { start: 0, end: 10 }],
            "a 10-token prompt under n_batch=512 must not be split"
        );
    }

    /// Exactly one absolute token position -- the prompt's true final token
    /// -- must ever request logits, and it must fall in the last chunk
    /// regardless of how many chunks the prompt was split into. This is the
    /// property `generate_inner` relies on for `batch.n_tokens() - 1` to
    /// index the right logits after the loop.
    #[test]
    fn test_only_the_prompts_true_final_token_requests_logits_across_chunk_boundaries() {
        let total_tokens = 1030;
        let chunks = plan_prompt_decode_chunks(0, total_tokens, 512);
        let logit_positions: Vec<usize> = chunks
            .iter()
            .flat_map(|chunk| chunk.start..chunk.end)
            .filter(|absolute_index| absolute_index + 1 == total_tokens)
            .collect();
        assert_eq!(
            logit_positions,
            vec![total_tokens - 1],
            "exactly one absolute position (the prompt's last token, {}) may request logits; \
             got {logit_positions:?} across chunks {chunks:?}",
            total_tokens - 1
        );
        let last_chunk = chunks.last().expect("chunks must be non-empty");
        assert!(
            (last_chunk.start..last_chunk.end).contains(&(total_tokens - 1)),
            "the logits-requesting position must fall in the last chunk {last_chunk:?}, \
             not an earlier one"
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

    /// Production-boundary regression for issue #1292: before this fix, a
    /// prompt longer than `n_batch` submitted its entire uncached suffix to
    /// one `decode()` call, which aborts the whole process with llama.cpp's
    /// native `GGML_ASSERT(n_tokens_all <= cparams.n_batch)` (`SIGABRT`, not
    /// a catchable Rust panic or `Result::Err`) -- confirmed live against
    /// this exact assertion with a 3000-token prompt before the chunking fix
    /// landed. `plan_prompt_decode_chunks` and `resolve_batch_sizes` above
    /// cover the chunking and batch-size arithmetic in isolation; only this
    /// test drives the real `LlamaCppGenerator::generate` entry point
    /// against the real native `decode()` call the abort came from, with a
    /// prompt deliberately built to exceed `LOCAL_N_BATCH` regardless of
    /// future tuning of that constant.
    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_CHAT pointing to a local chat GGUF"]
    fn test_real_gguf_chat_decodes_prompt_larger_than_n_batch_without_aborting() {
        let path = std::env::var("FINCH_TEST_GGUF_CHAT").expect("set FINCH_TEST_GGUF_CHAT");
        let mut generator = LlamaCppGenerator::load_with_offload(Path::new(&path), true, None)
            .expect("load GGUF chat");
        let seed = generator
            .tokenize("The quick brown fox jumps over the lazy dog and keeps running. ")
            .expect("tokenize seed phrase");
        assert!(
            !seed.is_empty(),
            "seed phrase must tokenize to at least one token"
        );
        // Repeat the seed (past its own leading BOS token) until the prompt
        // is comfortably past LOCAL_N_BATCH, so this exercises the chunked
        // decode path rather than the unrelated context-size guard.
        let target_len = usize::try_from(LOCAL_N_BATCH).expect("LOCAL_N_BATCH fits usize") + 500;
        let mut tokens = Vec::with_capacity(target_len);
        tokens.push(seed[0]);
        while tokens.len() < target_len {
            tokens.extend(seed.iter().skip(1).copied());
        }
        tokens.truncate(target_len);
        assert!(
            tokens.len() > usize::try_from(LOCAL_N_BATCH).expect("LOCAL_N_BATCH fits usize"),
            "test prompt ({}) must exceed LOCAL_N_BATCH ({LOCAL_N_BATCH}) to exercise chunking",
            tokens.len()
        );

        let output = generator
            .generate(&tokens, 4)
            .expect("a prompt longer than n_batch must decode across chunked calls, not abort");
        assert!(
            !output.is_empty(),
            "a real chat GGUF must emit at least one token for a valid, chunked prompt \
             (prompt_tokens={}, n_batch={LOCAL_N_BATCH})",
            tokens.len()
        );
    }
}
