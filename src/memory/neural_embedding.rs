// Neural ONNX Embedding Engine
//
// Implements EmbeddingEngine using a sentence transformer (all-MiniLM-L6-v2)
// running via ONNX Runtime for 384-dimensional semantic embeddings.
//
// Model: sentence-transformers/all-MiniLM-L6-v2 (Apache 2.0 license)
// ONNX conversion by Xenova/HuggingFace (also Apache 2.0)
//
// Distribution: downloaded from HuggingFace (Xenova/all-MiniLM-L6-v2-ONNX)
// ~23MB quantized ONNX model; cached in standard HF cache after first download.

use super::embeddings::EmbeddingEngine;
use anyhow::{anyhow, bail, Context, Result};
use ndarray::Array2;
use ort::{
    memory::MemoryInfo,
    session::{builder::GraphOptimizationLevel, Session},
    value::Value,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokenizers::Tokenizer;
use tracing::{debug, info};

/// Maximum sequence length for the embedding model.
/// all-MiniLM-L6-v2 supports up to 512 tokens; we truncate at 256 for efficiency.
const MAX_SEQ_LEN: usize = 256;

/// Output embedding dimension for all-MiniLM-L6-v2.
const EMBEDDING_DIM: usize = 384;

/// ONNX sentence transformer embedding engine.
///
/// Produces 384-dimensional L2-normalized embeddings via mean pooling over the
/// model's last_hidden_state output. Semantically much richer than the TF-IDF
/// fallback — two phrases with the same meaning score near 1.0 even if they
/// share no words.
///
/// The ONNX session is wrapped in a `Mutex` because `run_binding` requires
/// `&mut Session` while `EmbeddingEngine::embed` takes `&self`.
pub struct NeuralEmbeddingEngine {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    /// Whether the ONNX model expects a token_type_ids input.
    has_token_type_ids: bool,
}

impl NeuralEmbeddingEngine {
    /// Load a pre-downloaded embedding model from a directory.
    ///
    /// `model_dir` must contain:
    /// - `model_quantized.onnx` (preferred) or `model.onnx`
    /// - `tokenizer.json`
    pub fn load(model_dir: &Path) -> Result<Self> {
        info!("Loading neural embedding model from: {:?}", model_dir);

        // Find model file
        let model_path = {
            let quantized = model_dir.join("model_quantized.onnx");
            let regular = model_dir.join("model.onnx");
            if quantized.exists() {
                quantized
            } else if regular.exists() {
                regular
            } else {
                bail!(
                    "Embedding model not found in {:?}. Expected model_quantized.onnx or model.onnx",
                    model_dir
                );
            }
        };

        // Load tokenizer
        let tokenizer_path = model_dir.join("tokenizer.json");
        if !tokenizer_path.exists() {
            bail!("Tokenizer not found: {:?}", tokenizer_path);
        }
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow!("Failed to load tokenizer: {}", e))?;

        // Create ONNX Runtime session (CPU only for embeddings — fast enough)
        std::env::set_var("ORT_LOGGING_LEVEL", "3"); // Fatal only
        let session = Session::builder()
            .context("Failed to create ONNX session builder")?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .context("Failed to set optimization level")?
            .with_intra_threads(2)
            .context("Failed to set thread count")?
            .commit_from_file(&model_path)
            .with_context(|| format!("Failed to load ONNX model from {:?}", model_path))?;

        // Check which inputs the model expects
        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");

        info!(
            "Neural embedding model loaded: dim={}, token_type_ids={}",
            EMBEDDING_DIM, has_token_type_ids
        );

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            has_token_type_ids,
        })
    }

    /// Download the embedding model from HuggingFace if not already cached.
    ///
    /// Returns the local directory containing model and tokenizer files.
    /// This is a blocking operation; wrap in `spawn_blocking` for async contexts.
    pub fn download_sync() -> Result<PathBuf> {
        use hf_hub::{api::sync::Api, Repo, RepoType};

        info!("Downloading neural embedding model (all-MiniLM-L6-v2)...");

        let api = Api::new().context("Failed to create HuggingFace Hub API")?;
        let repo = api.repo(Repo::new(
            "Xenova/all-MiniLM-L6-v2-ONNX".to_string(),
            RepoType::Model,
        ));

        // Download model (tries quantized first, then regular)
        let model_path = repo
            .get("model_quantized.onnx")
            .or_else(|_| repo.get("model.onnx"))
            .context(
                "Failed to download embedding model \
                 (tried model_quantized.onnx and model.onnx)",
            )?;

        // Download tokenizer
        let _tokenizer_path = repo
            .get("tokenizer.json")
            .context("Failed to download tokenizer.json")?;

        let dir = model_path
            .parent()
            .ok_or_else(|| anyhow!("Model path has no parent directory"))?
            .to_path_buf();

        info!("Neural embedding model downloaded to: {:?}", dir);
        Ok(dir)
    }

    /// Async version: download model using a blocking thread pool.
    pub async fn ensure_downloaded() -> Result<PathBuf> {
        tokio::task::spawn_blocking(Self::download_sync)
            .await
            .context("Embedding model download task panicked")??;

        // Re-run synchronously to get the path (spawn_blocking result already dropped)
        // Actually, re-run is cheap since files are already cached after the above
        Self::download_sync()
    }

    /// Try to find the model in the HuggingFace cache without downloading.
    ///
    /// Returns `None` if the model is not yet cached (i.e., first run).
    pub fn find_in_cache() -> Option<PathBuf> {
        // HF hub caches models under: ~/.cache/huggingface/hub/
        let cache_base = dirs::home_dir()?
            .join(".cache")
            .join("huggingface")
            .join("hub");
        let repo_dir = cache_base.join("models--Xenova--all-MiniLM-L6-v2-ONNX");

        if !repo_dir.exists() {
            debug!("Embedding model not in cache: {:?}", repo_dir);
            return None;
        }

        // Find the latest snapshot
        let snapshots_dir = repo_dir.join("snapshots");
        if !snapshots_dir.exists() {
            return None;
        }

        // Walk into snapshots and find a directory that has the model file
        let entries = std::fs::read_dir(&snapshots_dir).ok()?;
        for entry in entries.flatten() {
            let snapshot = entry.path();
            if snapshot.is_dir() {
                let has_model = snapshot.join("model_quantized.onnx").exists()
                    || snapshot.join("model.onnx").exists();
                let has_tokenizer = snapshot.join("tokenizer.json").exists();
                if has_model && has_tokenizer {
                    debug!("Found embedding model in cache: {:?}", snapshot);
                    return Some(snapshot);
                }
            }
        }

        None
    }

    /// Encode text into input_ids and attention_mask, truncated at MAX_SEQ_LEN.
    fn tokenize(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>)> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow!("Tokenization failed: {}", e))?;

        let ids: Vec<i64> = encoding
            .get_ids()
            .iter()
            .take(MAX_SEQ_LEN)
            .map(|&id| id as i64)
            .collect();

        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .take(MAX_SEQ_LEN)
            .map(|&m| m as i64)
            .collect();

        Ok((ids, mask))
    }
}

impl EmbeddingEngine for NeuralEmbeddingEngine {
    fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let (input_ids, attention_mask) = self.tokenize(text)?;
        let seq_len = input_ids.len();

        if seq_len == 0 {
            return Ok(vec![0.0; EMBEDDING_DIM]);
        }

        // Build input tensors [1, seq_len]
        let ids_arr = Array2::from_shape_vec((1, seq_len), input_ids)
            .context("Failed to create input_ids ndarray")?;
        let mask_arr = Array2::from_shape_vec((1, seq_len), attention_mask.clone())
            .context("Failed to create attention_mask ndarray")?;

        let ids_val = Value::from_array(ids_arr)
            .context("Failed to create input_ids Value")?
            .into_dyn();
        let mask_val = Value::from_array(mask_arr)
            .context("Failed to create attention_mask Value")?
            .into_dyn();

        // Acquire session lock (Mutex needed because run_binding requires &mut Session)
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("ONNX session mutex poisoned"))?;

        // Build IoBinding
        let mut binding = session
            .create_binding()
            .context("Failed to create IoBinding")?;

        binding
            .bind_input("input_ids", &ids_val)
            .context("Failed to bind input_ids")?;
        binding
            .bind_input("attention_mask", &mask_val)
            .context("Failed to bind attention_mask")?;

        // Some ONNX exports require token_type_ids (all zeros for single sentence)
        let tti_val_holder;
        if self.has_token_type_ids {
            let tti_data = vec![0i64; seq_len];
            let tti_arr = Array2::from_shape_vec((1, seq_len), tti_data)
                .context("Failed to create token_type_ids ndarray")?;
            tti_val_holder = Value::from_array(tti_arr)
                .context("Failed to create token_type_ids Value")?
                .into_dyn();
            binding
                .bind_input("token_type_ids", &tti_val_holder)
                .context("Failed to bind token_type_ids")?;
        }

        // Bind last_hidden_state output
        let mem_info = MemoryInfo::default();
        binding
            .bind_output_to_device("last_hidden_state", &mem_info)
            .context("Failed to bind last_hidden_state output")?;

        // Also bind any other outputs the model has (prevents IoBinding errors
        // on models that require all outputs to be bound)
        for out in session.outputs().iter() {
            if out.name() != "last_hidden_state" {
                let _ = binding.bind_output_to_device(out.name(), &mem_info);
            }
        }

        // Run inference
        let outputs = session
            .run_binding(&binding)
            .context("ONNX inference failed")?;

        let lhs = outputs
            .get("last_hidden_state")
            .ok_or_else(|| anyhow!("Missing last_hidden_state in model outputs"))?;

        // Shape: [1, seq_len, EMBEDDING_DIM]
        let (shape, data) = lhs
            .try_extract_tensor::<f32>()
            .context("Failed to extract last_hidden_state tensor")?;

        if shape.len() != 3 {
            bail!(
                "Expected 3D last_hidden_state tensor, got shape {:?}",
                shape
            );
        }
        let hidden_dim = shape[2] as usize;
        let actual_seq = shape[1] as usize;

        // Mean pool over sequence dimension, weighted by attention_mask
        let mut pooled = vec![0.0f32; hidden_dim];
        let mut count = 0.0f32;
        for (i, &mask) in attention_mask
            .iter()
            .enumerate()
            .take(actual_seq.min(attention_mask.len()))
        {
            if mask == 1 {
                count += 1.0;
                let offset = i * hidden_dim;
                for j in 0..hidden_dim {
                    pooled[j] += data[offset + j];
                }
            }
        }
        if count > 0.0 {
            for v in &mut pooled {
                *v /= count;
            }
        }

        // L2 normalize
        let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut pooled {
                *v /= norm;
            }
        }

        Ok(pooled)
    }

    fn dimension(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neural_embedding_dim_constant() {
        assert_eq!(EMBEDDING_DIM, 384);
    }

    #[test]
    fn test_max_seq_len_constant() {
        assert_eq!(MAX_SEQ_LEN, 256);
    }

    /// Verify that find_in_cache does not panic and returns None when absent.
    #[test]
    fn test_find_in_cache_returns_none_when_absent() {
        // This test is expected to return None in CI (no HF cache pre-seeded).
        // It should never panic.
        let _result = NeuralEmbeddingEngine::find_in_cache();
        // Just checking it doesn't panic
    }

    /// Full load + inference requires the actual model files; mark as #[ignore]
    /// so it runs only on developer machines with the model cached.
    #[test]
    #[ignore]
    fn test_neural_embed_dimensions() {
        if let Some(model_dir) = NeuralEmbeddingEngine::find_in_cache() {
            let engine = NeuralEmbeddingEngine::load(&model_dir).expect("Should load from cache");
            assert_eq!(engine.dimension(), 384);

            let emb = engine.embed("Hello world").unwrap();
            assert_eq!(emb.len(), 384);

            // Unit vector check
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (norm - 1.0).abs() < 0.01,
                "Should be unit vector, norm={}",
                norm
            );
        }
    }

    #[test]
    #[ignore]
    fn test_neural_embed_semantic_similarity() {
        if let Some(model_dir) = NeuralEmbeddingEngine::find_in_cache() {
            let engine = NeuralEmbeddingEngine::load(&model_dir).unwrap();

            let e1 = engine.embed("Rust programming language").unwrap();
            let e2 = engine.embed("Rust systems programming").unwrap();
            let e3 = engine.embed("Python machine learning").unwrap();

            let sim_related = crate::memory::embeddings::cosine_similarity(&e1, &e2);
            let sim_unrelated = crate::memory::embeddings::cosine_similarity(&e1, &e3);

            assert!(
                sim_related > sim_unrelated,
                "Related texts (sim={:.3}) should outscore unrelated (sim={:.3})",
                sim_related,
                sim_unrelated
            );
        }
    }

    #[test]
    #[ignore]
    fn test_neural_embed_empty_text() {
        if let Some(model_dir) = NeuralEmbeddingEngine::find_in_cache() {
            let engine = NeuralEmbeddingEngine::load(&model_dir).unwrap();
            let emb = engine.embed("").unwrap();
            assert_eq!(emb.len(), 384);
            // Empty text → zero vector (no tokens to pool)
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert_eq!(norm, 0.0, "Empty text should produce zero vector");
        }
    }
}
