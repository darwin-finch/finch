// Generator Model - Text generation
// Generates Claude-style responses using autoregressive decoding

use anyhow::Result;
use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{
    embedding, layer_norm, linear, Embedding, LayerNorm, Linear, Optimizer, VarBuilder, VarMap, SGD,
};
use std::path::Path;

use super::common::{get_device_with_preference, ModelConfig, Saveable};

/// Generator model - autoregressive text generation
pub struct GeneratorModel {
    embedding: Embedding,
    decoder: TransformerDecoder,
    lm_head: Linear,
    device: Device,
    varmap: VarMap,
    max_length: usize,
}

impl GeneratorModel {
    /// Create new generator with random initialization
    pub fn new(config: &ModelConfig) -> Result<Self> {
        let device = get_device_with_preference(config.device_preference)?;
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        // Embedding layer
        let embedding = embedding(config.vocab_size, config.hidden_dim, vb.pp("embedding"))?;

        // Transformer decoder
        let decoder = TransformerDecoder::new(config, vb.pp("decoder"))?;

        // Language model head (hidden_dim → vocab_size)
        let lm_head = linear(config.hidden_dim, config.vocab_size, vb.pp("lm_head"))?;

        Ok(Self {
            embedding,
            decoder,
            lm_head,
            device,
            varmap,
            max_length: config.max_seq_len,
        })
    }

    /// Forward pass: input_ids → logits for next token
    pub fn forward(&self, input_ids: &Tensor) -> Result<Tensor> {
        // Embed tokens
        let embedded = self.embedding.forward(input_ids)?;

        // Decode with transformer
        let decoded = self.decoder.forward(&embedded)?;

        // Project to vocabulary (logits for next token)
        let logits = self.lm_head.forward(&decoded)?;

        Ok(logits)
    }

    /// Generate response text (autoregressive sampling)
    pub fn generate(&self, input_ids: &Tensor, max_new_tokens: usize) -> Result<Vec<u32>> {
        let mut generated = input_ids.to_vec2::<u32>()?[0].clone();

        for _ in 0..max_new_tokens {
            // Forward pass on current sequence
            let current = Tensor::from_vec(generated.clone(), (1, generated.len()), &self.device)?;
            let logits = self.forward(&current)?;

            // Get logits for last token
            let last_logits = logits.i((0, generated.len() - 1))?;

            // Sample next token (greedy for now)
            let next_token = last_logits.argmax(0)?.to_scalar::<u32>()?;

            // Stop if EOS token (assume token 2 is EOS)
            if next_token == 2 {
                break;
            }

            generated.push(next_token);

            // Stop if max length reached
            if generated.len() >= self.max_length {
                break;
            }
        }

        Ok(generated)
    }

    /// Train on a single example (online learning)
    pub fn update(
        &mut self,
        input_ids: &Tensor,
        target_ids: &[u32],
        learning_rate: f64,
    ) -> Result<()> {
        // Forward pass
        let logits = self.forward(input_ids)?;

        // Compute cross-entropy loss
        let target_tensor = Tensor::new(target_ids, &self.device)?;
        let loss = cross_entropy_loss(&logits, &target_tensor)?;

        // Backward pass + parameter update (Candle does both in one call)
        let mut optimizer = candle_nn::SGD::new(self.varmap.all_vars(), learning_rate)?;
        optimizer.backward_step(&loss)?;

        Ok(())
    }
}

/// Transformer decoder (for text generation)
struct TransformerDecoder {
    layers: Vec<DecoderLayer>,
}

impl TransformerDecoder {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        let mut layers = Vec::new();
        for i in 0..config.num_layers {
            layers.push(DecoderLayer::new(config, vb.pp(&format!("layer_{}", i)))?);
        }
        Ok(Self { layers })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mut hidden = x.clone();
        for layer in &self.layers {
            hidden = layer.forward(&hidden)?;
        }
        Ok(hidden)
    }
}

/// Single decoder layer (with causal masking for autoregressive generation)
struct DecoderLayer {
    self_attn: CausalSelfAttention,
    feed_forward: FeedForward,
    norm1: LayerNorm,
    norm2: LayerNorm,
}

impl DecoderLayer {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            self_attn: CausalSelfAttention::new(config, vb.pp("self_attn"))?,
            feed_forward: FeedForward::new(config, vb.pp("ffn"))?,
            norm1: layer_norm(config.hidden_dim, 1e-5, vb.pp("norm1"))?,
            norm2: layer_norm(config.hidden_dim, 1e-5, vb.pp("norm2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        // Self-attention with residual
        let attn_out = self.self_attn.forward(x)?;
        let x = (x + attn_out)?;
        let x = self.norm1.forward(&x)?;

        // Feed-forward with residual
        let ffn_out = self.feed_forward.forward(&x)?;
        let x = (&x + ffn_out)?;
        let x = self.norm2.forward(&x)?;

        Ok(x)
    }
}

/// Causal self-attention (for autoregressive generation)
struct CausalSelfAttention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    head_dim: usize,
}

impl CausalSelfAttention {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        let head_dim = config.hidden_dim / config.num_heads;
        Ok(Self {
            q_proj: linear(config.hidden_dim, config.hidden_dim, vb.pp("q"))?,
            k_proj: linear(config.hidden_dim, config.hidden_dim, vb.pp("k"))?,
            v_proj: linear(config.hidden_dim, config.hidden_dim, vb.pp("v"))?,
            o_proj: linear(config.hidden_dim, config.hidden_dim, vb.pp("o"))?,
            head_dim,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let q = self.q_proj.forward(x)?;
        let k = self.k_proj.forward(x)?;
        let v = self.v_proj.forward(x)?;

        // Compute attention scores
        let scores = q.matmul(&k.t()?)?;
        let scale = (self.head_dim as f64).sqrt();
        let scores = (scores / scale)?;

        // Apply causal mask (prevent attending to future tokens)
        let seq_len = scores.dim(1)?;
        let mask = create_causal_mask(seq_len, scores.device())?;
        let scores_masked = scores.broadcast_add(&mask)?;

        // Softmax and apply to values
        let attn_weights = candle_nn::ops::softmax(&scores_masked, 1)?;
        let attn_out = attn_weights.matmul(&v)?;

        Ok(self.o_proj.forward(&attn_out)?)
    }
}

/// Feed-forward network
struct FeedForward {
    linear1: Linear,
    linear2: Linear,
}

impl FeedForward {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        let hidden = config.hidden_dim * 4;
        Ok(Self {
            linear1: linear(config.hidden_dim, hidden, vb.pp("fc1"))?,
            linear2: linear(hidden, config.hidden_dim, vb.pp("fc2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.linear1.forward(x)?;
        let x = gelu(&x)?;
        Ok(self.linear2.forward(&x)?)
    }
}

/// Create causal mask for autoregressive generation
fn create_causal_mask(seq_len: usize, device: &Device) -> Result<Tensor> {
    let mut mask_data = vec![0.0f32; seq_len * seq_len];
    for i in 0..seq_len {
        for j in (i + 1)..seq_len {
            mask_data[i * seq_len + j] = f32::NEG_INFINITY;
        }
    }
    Ok(Tensor::from_vec(mask_data, (seq_len, seq_len), device)?)
}

/// Cross-entropy loss for language modeling
fn cross_entropy_loss(logits: &Tensor, targets: &Tensor) -> Result<Tensor> {
    // Flatten logits to (batch_size * seq_len, vocab_size)
    let vocab_size = logits.dim(logits.dims().len() - 1)?;
    let logits_flat = logits.reshape(((), vocab_size))?;

    // Apply log softmax
    let log_probs = candle_nn::ops::log_softmax(&logits_flat, 1)?;

    // Gather log probabilities for target tokens
    // TODO: Implement proper cross-entropy calculation
    // For now, return mean of log_probs as placeholder
    Ok(log_probs.mean_all()?)
}

/// GELU activation
fn gelu(x: &Tensor) -> Result<Tensor> {
    let sig_input = (x * 1.702)?;
    let sig = sigmoid(&sig_input)?;
    Ok((x * sig)?)
}

/// Sigmoid activation
fn sigmoid(x: &Tensor) -> Result<Tensor> {
    let neg_x = x.neg()?;
    let exp_neg_x = neg_x.exp()?;
    let one_plus_exp = (exp_neg_x + 1.0)?;
    Ok((one_plus_exp.recip())?)
}

impl Saveable for GeneratorModel {
    fn save(&self, path: &Path) -> Result<()> {
        self.varmap.save(path)?;
        Ok(())
    }

    fn load(_path: &Path) -> Result<Self> {
        // TODO: Implement proper loading
        unimplemented!("Generator model loading not yet implemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generator_creation() {
        let config = ModelConfig::default();
        let generator = GeneratorModel::new(&config);
        assert!(generator.is_ok());
    }

    #[test]
    fn test_generator_forward() -> Result<()> {
        let config = ModelConfig::default();
        let generator = GeneratorModel::new(&config)?;

        // Create dummy input (batch_size=1, seq_len=10)
        let input_ids = Tensor::zeros((1, 10), DType::U32, &generator.device)?;

        let output = generator.forward(&input_ids)?;
        // Output should be (batch_size, seq_len, vocab_size)
        assert_eq!(output.dims(), &[1, 10, config.vocab_size]);

        Ok(())
    }
}
