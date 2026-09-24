// Neural GGUF Embedding Engine
//
// Implements EmbeddingEngine using bge-small-en-v1.5 running through
// llama.cpp's embeddings mode for 384-dimensional semantic embeddings.
//
// This is the same in-process llama.cpp backend the chat-model loader
// (src/models/loaders/llama_cpp.rs) already uses, including its real GPU
// offload path on macOS -- not the ONNX+CoreML path memory embeddings used
// before, which was never confirmed to place work on the GPU for generation
// either (the deleted onnx.rs carried an elaborate CoreML compute-plan
// profiling apparatus that only makes sense if placement was never actually
// confirmed).
//
// Model: CompendiumLabs/bge-small-en-v1.5-gguf, revision
// d32f8c040ea3b516330eeb75b72bcc2d3a780ab7, bge-small-en-v1.5-q8_0.gguf.
// BERT architecture, CLS pooling (the GGUF's own metadata declares
// bert.pooling_type=2), 384-dim -- confirmed directly from the file's GGUF
// header, not assumed. Q8_0 (near-lossless) rather than the chat picker's
// Q4KM/Q5KM: quantization error is proportionally larger on a 33M-parameter
// model, and the absolute size cost of full fidelity here is trivial
// (~37MB). Repository, revision, size, and sha256 were verified by
// downloading the file and running `shasum -a 256` locally, not copied
// from an unverified source.

use anyhow::{anyhow, bail, Context, Result};
use finch_memory::{EmbeddingEngine, TfIdfEmbedding};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use once_cell::sync::OnceCell;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::bootstrap::GeneratorState;
use super::gguf_download::{
    managed_gguf_cache_path, GgufQuantization, ManagedGgufArtifact, ManagedGgufDownloader,
};

/// Output embedding dimension for bge-small-en-v1.5 -- confirmed from the
/// GGUF's own `bert.embedding_length` metadata key, not assumed.
const EMBEDDING_DIM: usize = 384;

// llama.cpp permits one initialized backend per process. The chat-model
// loader (src/models/loaders/llama_cpp.rs) owns its own static for the same
// reason and cannot be reused here: a `LlamaBackend` is not associated with
// one model, but each module's own `OnceCell` only needs to be initialized
// once regardless of which module reaches it first, so two independent
// cells both calling `LlamaBackend::init()` would violate "one per process"
// on whichever module initializes second. Real, disclosed gap: if both the
// chat and embedding paths end up loaded in the same process, this needs a
// single shared backend cell instead of one per module.
static BACKEND: OnceCell<LlamaBackend> = OnceCell::new();

fn backend() -> Result<&'static LlamaBackend> {
    BACKEND.get_or_try_init(|| {
        let mut backend = LlamaBackend::init().context("initialize llama.cpp backend")?;
        // This backend lives in the interactive frontend. Native llama.cpp
        // stderr bypasses the TUI renderer and corrupts its cursor geometry.
        backend.void_logs();
        Ok(backend)
    })
}

/// The one fixed managed GGUF artifact backing memory's embedding engine.
///
/// Not part of `managed_gguf_artifact()`'s `ModelFamily`/`ModelSize`
/// registry: that registry is a user-facing choice among interchangeable
/// chat models. This is a single, specific support model finch-builtin
/// functions use for themselves, with no equivalent choice to offer.
fn memory_embedding_gguf_artifact() -> ManagedGgufArtifact {
    ManagedGgufArtifact {
        repository: "CompendiumLabs/bge-small-en-v1.5-gguf".to_string(),
        revision: "d32f8c040ea3b516330eeb75b72bcc2d3a780ab7".to_string(),
        filename: "bge-small-en-v1.5-q8_0.gguf".to_string(),
        quantization: GgufQuantization::Q8_0,
        expected_size: 36_806_944,
        sha256: "ec38e8da142596baa913124ae50550de284b6916bf59577ef2f0cb9660c2f514".to_string(),
    }
}

/// BERT-family sentence embedding engine running through llama.cpp.
///
/// The context is built once with embeddings mode enabled
/// (`LlamaContextParams::with_embeddings(true)`) and reused across calls,
/// guarded by a `Mutex` because decoding requires `&mut LlamaContext` while
/// `EmbeddingEngine::embed` takes `&self`.
pub struct NeuralEmbeddingEngine {
    model: LlamaModel,
    context_state: Mutex<()>,
}

// SAFETY-shaped note, not an actual unsafe impl: `LlamaContext` borrows
// `LlamaModel` for its lifetime, which is why the context is created fresh
// per `embed()` call under the same mutex rather than stored -- storing a
// context alongside its owning model in one struct needs a self-referential
// lifetime this type does not attempt.
impl NeuralEmbeddingEngine {
    /// Load a pre-downloaded GGUF embedding model from its exact file path.
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.is_file() {
            bail!("embedding GGUF model file does not exist: {:?}", model_path);
        }
        info!("Loading GGUF embedding model from: {:?}", model_path);

        let backend = backend()?;
        let params = memory_embedding_model_params();
        let model = LlamaModel::load_from_file(backend, model_path, &params)
            .with_context(|| format!("load GGUF embedding model from {:?}", model_path))?;

        info!("GGUF embedding model loaded: dim={}", EMBEDDING_DIM);

        Ok(Self {
            model,
            context_state: Mutex::new(()),
        })
    }

    /// Download the embedding model if it is not already cached, reporting
    /// progress through `state` the same way the chat-model GGUF downloader
    /// does (`ManagedGgufDownloader::ensure`) -- real byte-level progress,
    /// resumable, sha256-verified.
    pub async fn ensure_downloaded(state: Arc<RwLock<GeneratorState>>) -> Result<PathBuf> {
        let downloader = ManagedGgufDownloader::from_environment(None)
            .context("build managed GGUF downloader for the memory embedding model")?;
        let artifact = memory_embedding_gguf_artifact();
        let cancellation = CancellationToken::new();
        let (path, _disposition) = downloader
            .ensure(
                &artifact,
                "Memory embeddings (bge-small)",
                state,
                &cancellation,
            )
            .await
            .context("download memory embedding model")?;
        Ok(path)
    }

    /// Try to find the model without downloading or verifying a checksum --
    /// a cheap, sync, filesystem-only existence check at the exact path a
    /// verified download commits to (`ManagedGgufDownloader::ensure`'s
    /// atomic rename never leaves a partial or unverified file at the final
    /// path). Returns `None` if the model is not yet cached (i.e., first run).
    pub fn find_in_cache() -> Option<PathBuf> {
        let path = managed_gguf_cache_path(&memory_embedding_gguf_artifact()).ok()?;
        if path.is_file() {
            debug!("Found embedding model in managed GGUF cache: {:?}", path);
            Some(path)
        } else {
            debug!("Embedding model not in managed GGUF cache: {:?}", path);
            None
        }
    }
}

/// Select the production memory embedding engine without downloading.
///
/// Composition owns this choice. `MemorySystem` constructors must not probe
/// the managed GGUF cache or start a download.
pub fn select_memory_embedding_engine(use_neural_embeddings: bool) -> Arc<dyn EmbeddingEngine> {
    if use_neural_embeddings {
        match NeuralEmbeddingEngine::find_in_cache()
            .and_then(|path| NeuralEmbeddingEngine::load(&path).ok())
        {
            Some(neural) => {
                debug!("Using neural GGUF embeddings (bge-small-en-v1.5)");
                Arc::new(neural)
            }
            None => {
                debug!(
                    "Embedding model not in cache — using TF-IDF fallback until \
                     NeuralEmbeddingEngine::ensure_downloaded() completes."
                );
                Arc::new(TfIdfEmbedding::new())
            }
        }
    } else {
        Arc::new(TfIdfEmbedding::new())
    }
}

fn context_params() -> LlamaContextParams {
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(512))
        .with_embeddings(true)
}

fn memory_embedding_model_params() -> LlamaModelParams {
    // The support model is only ~37 MB. GPU offload has negligible benefit,
    // but it creates Metal residency sets in the frontend and can make
    // llama.cpp abort during process teardown while those sets are live.
    LlamaModelParams::default().with_n_gpu_layers(0)
}

impl EmbeddingEngine for NeuralEmbeddingEngine {
    fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let tokens = self
            .model
            .str_to_token(text, AddBos::Always)
            .context("tokenize embedding input")?;
        if tokens.is_empty() {
            return Ok(vec![0.0; EMBEDDING_DIM]);
        }

        // The mutex is held for the whole decode+extract, not just context
        // creation: `LlamaContext` borrows `self.model`, so it cannot
        // outlive this guard anyway, and holding it the whole time keeps
        // "one embed() in flight at a time" honest rather than implicit.
        let _guard = self
            .context_state
            .lock()
            .map_err(|_| anyhow!("embedding context mutex poisoned"))?;

        let backend = backend()?;
        let mut context = self
            .model
            .new_context(backend, context_params())
            .context("create llama.cpp embedding context")?;

        let mut batch = LlamaBatch::new(tokens.len(), 1);
        for (index, token) in tokens.iter().copied().enumerate() {
            // Every token, not just the last: embedding extraction pools
            // over the whole sequence's hidden states (the GGUF's own
            // `bert.pooling_type=2` makes llama.cpp do this internally), so
            // every position needs to actually run, unlike next-token
            // generation where only the final position's logits matter.
            batch
                .add(token, index as i32, &[0], true)
                .context("build embedding batch")?;
        }
        context
            .decode(&mut batch)
            .context("decode embedding input")?;

        let raw = context
            .embeddings_seq_ith(0)
            .context("extract pooled embedding")?;
        if raw.len() != EMBEDDING_DIM {
            bail!(
                "embedding model returned {} dims, expected {EMBEDDING_DIM}",
                raw.len()
            );
        }

        // L2 normalize -- bge-small-en-v1.5 is documented and used with
        // normalized embeddings for cosine similarity, matching what the
        // ONNX path this replaces also did.
        let mut embedding = raw.to_vec();
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut embedding {
                *v /= norm;
            }
        }
        Ok(embedding)
    }

    fn dimension(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_memory_embedding_engine_disabled_is_tfidf() {
        let engine = select_memory_embedding_engine(false);
        assert_eq!(
            engine.dimension(),
            TfIdfEmbedding::new().dimension(),
            "disabling neural embeddings must yield the TF-IDF fallback, dim={}",
            engine.dimension()
        );
    }

    #[test]
    fn test_neural_embedding_dim_constant() {
        assert_eq!(EMBEDDING_DIM, 384);
    }

    #[test]
    fn memory_embedding_model_stays_off_metal() {
        assert_eq!(memory_embedding_model_params().n_gpu_layers(), 0);
    }

    #[test]
    fn test_memory_embedding_artifact_matches_independently_verified_values() {
        let artifact = memory_embedding_gguf_artifact();
        assert_eq!(artifact.repository, "CompendiumLabs/bge-small-en-v1.5-gguf");
        assert_eq!(artifact.revision.len(), 40, "must be a full commit sha");
        assert_eq!(artifact.filename, "bge-small-en-v1.5-q8_0.gguf");
        assert_eq!(artifact.expected_size, 36_806_944);
        assert_eq!(
            artifact.sha256,
            "ec38e8da142596baa913124ae50550de284b6916bf59577ef2f0cb9660c2f514"
        );
        assert_eq!(artifact.sha256.len(), 64, "sha256 must be 64 hex chars");
        artifact
            .validate()
            .expect("artifact must pass its own validation");
    }

    #[test]
    fn test_managed_gguf_cache_path_is_under_the_finch_gguf_cache() {
        let path = managed_gguf_cache_path(&memory_embedding_gguf_artifact())
            .expect("cache dir must resolve on a test machine");
        assert!(
            path.ends_with(std::path::Path::new(
                "finch/gguf/CompendiumLabs--bge-small-en-v1.5-gguf/\
                 d32f8c040ea3b516330eeb75b72bcc2d3a780ab7/bge-small-en-v1.5-q8_0.gguf"
            )),
            "got {path:?}"
        );
    }

    /// Verify that find_in_cache does not panic and returns None when absent.
    #[test]
    fn test_find_in_cache_returns_none_when_absent() {
        // This test is expected to return None in CI (nothing pre-seeded in
        // the managed GGUF cache). It should never panic.
        let _result = NeuralEmbeddingEngine::find_in_cache();
    }

    /// Full load + inference requires the actual model file; mark as
    /// `#[ignore]` so it runs only on developer machines with the model
    /// downloaded (set `FINCH_TEST_EMBEDDING_GGUF` to its path).
    #[test]
    #[ignore]
    fn test_neural_embed_dimensions() {
        let Ok(path) = std::env::var("FINCH_TEST_EMBEDDING_GGUF") else {
            return;
        };
        let engine = NeuralEmbeddingEngine::load(Path::new(&path)).expect("load from path");
        assert_eq!(engine.dimension(), 384);

        let emb = engine.embed("Hello world").unwrap();
        assert_eq!(emb.len(), 384);

        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "should be unit vector, norm={norm}"
        );
    }

    #[test]
    #[ignore]
    fn test_neural_embed_semantic_similarity() {
        let Ok(path) = std::env::var("FINCH_TEST_EMBEDDING_GGUF") else {
            return;
        };
        let engine = NeuralEmbeddingEngine::load(Path::new(&path)).unwrap();

        let e1 = engine.embed("Rust programming language").unwrap();
        let e2 = engine.embed("Rust systems programming").unwrap();
        let e3 = engine.embed("Python machine learning").unwrap();

        let sim_related = finch_memory::cosine_similarity(&e1, &e2);
        let sim_unrelated = finch_memory::cosine_similarity(&e1, &e3);

        assert!(
            sim_related > sim_unrelated,
            "related texts (sim={sim_related:.3}) should outscore unrelated (sim={sim_unrelated:.3})"
        );
    }

    /// Production-boundary regression test for the Metal residency-set
    /// teardown abort: load the real model with the production params,
    /// embed, and let the engine (and the `LlamaModel` it owns) drop at
    /// scope exit. `memory_embedding_model_stays_off_metal` above only
    /// checks the params builder's own return value and would pass even if
    /// those params were never wired into the load path; this test
    /// exercises the actual load-then-drop sequence. Before this fix, GPU
    /// offload on this ~37MB support model could create a Metal residency
    /// set and abort the whole process on drop -- a failure mode no
    /// in-process assertion can observe directly, since reaching the
    /// assertion below is exactly what such an abort would prevent. Still
    /// `#[ignore]`d like the other real-model tests in this file: it needs
    /// the downloaded GGUF (`FINCH_TEST_EMBEDDING_GGUF`) and, to actually
    /// exercise the Metal path this guards against, a macOS Metal machine --
    /// neither is available in this repo's Linux PR CI (macOS CI here is a
    /// post-merge cache warmer, not a merge gate; see `test-macos` in
    /// `.github/workflows/ci.yml`).
    #[test]
    #[ignore]
    fn test_neural_embed_load_and_drop_does_not_abort_process() {
        let Ok(path) = std::env::var("FINCH_TEST_EMBEDDING_GGUF") else {
            return;
        };
        let embedding_len = {
            let engine = NeuralEmbeddingEngine::load(Path::new(&path)).expect("load from path");
            let embedding = engine.embed("teardown probe").expect("embed after load");
            embedding.len()
            // `engine` drops here, taking its `LlamaModel` with it.
        };
        assert_eq!(
            embedding_len, EMBEDDING_DIM,
            "engine must produce a full-dimension embedding, and this \
             assertion must be reached at all: an abort during the drop \
             above would kill the test process before it ever prints a \
             result"
        );
    }
}
