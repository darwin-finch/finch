// Validator Model - Quality assessment
// Checks if generated response is good enough (binary: 0=bad, 1=good)

use anyhow::Result;
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{
    embedding, layer_norm, linear, Embedding, LayerNorm, Linear, Optimizer, VarBuilder, VarMap, SGD,
};
use std::path::Path;

use super::common::{get_device_with_preference, ModelConfig, Saveable};

/// Validator model - binary quality classifier
pub struct ValidatorModel {
    embedding: Embedding,
    encoder: TransformerEncoder,
    classifier: Linear,
    device: Device,
    varmap: VarMap,
}

impl ValidatorModel {
    /// Create new validator with random initialization
    pub fn new(config: &ModelConfig) -> Result<Self> {
        let device = get_device_with_preference(config.device_preference)?;
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        // Embedding layer
        let embedding = embedding(config.vocab_size, config.hidden_dim, vb.pp("embedding"))?;

        // Transformer encoder
        let encoder = TransformerEncoder::new(config, vb.pp("encoder"))?;

        // Classification head (hidden_dim → 1)
        let classifier = linear(config.hidden_dim, 1, vb.pp("classifier"))?;

        Ok(Self {
            embedding,
            encoder,
            classifier,
            device,
            varmap,
        })
    }

    /// Forward pass: concatenated (query + response) → quality score
    pub fn forward(&self, input_ids: &Tensor) -> Result<Tensor> {
        // Embed tokens
        let embedded = self.embedding.forward(input_ids)?;

        // Encode with transformer
        let encoded = self.encoder.forward(&embedded)?;

        // Pool (mean over sequence)
        let pooled = encoded.mean(1)?;

        // Classify (→ single logit)
        let logit = self.classifier.forward(&pooled)?;

        // Sigmoid → probability
        sigmoid(&logit)
    }

    /// Validate response quality: 0 (bad) or 1 (good)
    pub fn validate(&self, query_ids: &Tensor, response_ids: &Tensor) -> Result<bool> {
        // Concatenate query and response
        let combined = Tensor::cat(&[query_ids, response_ids], 1)?;

        let prob = self.forward(&combined)?;
        // Squeeze to get scalar from [1, 1] tensor
        let prob_squeezed = prob.squeeze(0)?.squeeze(0)?;
        let prob_scalar = prob_squeezed.to_scalar::<f32>()?;

        // Decision: probability > 0.5
        Ok(prob_scalar > 0.5)
    }

    /// Update weights based on actual quality (online learning)
    pub fn update(
        &mut self,
        query_ids: &Tensor,
        response_ids: &Tensor,
        is_good: bool,
        learning_rate: f64,
    ) -> Result<()> {
        // Concatenate query and response
        let combined = Tensor::cat(&[query_ids, response_ids], 1)?;

        // Forward pass
        let pred = self.forward(&combined)?;

        // Compute loss (binary cross-entropy)
        // Target should match pred shape (batch_size, 1)
        let target_val = if is_good { 1.0f32 } else { 0.0f32 };
        let target_tensor = Tensor::from_vec(vec![target_val], (1, 1), &self.device)?;
        let loss = binary_cross_entropy(&pred, &target_tensor)?;

        // Backward pass + parameter update (Candle does both in one call)
        let mut optimizer = candle_nn::SGD::new(self.varmap.all_vars(), learning_rate)?;
        optimizer.backward_step(&loss)?;

        Ok(())
    }
}

/// Transformer encoder (similar to Router)
struct TransformerEncoder {
    layers: Vec<TransformerLayer>,
}

impl TransformerEncoder {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        let mut layers = Vec::new();
        for i in 0..config.num_layers {
            layers.push(TransformerLayer::new(
                config,
                vb.pp(&format!("layer_{}", i)),
            )?);
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

/// Single transformer layer
struct TransformerLayer {
    self_attn: MultiHeadAttention,
    feed_forward: FeedForward,
    norm1: LayerNorm,
    norm2: LayerNorm,
}

impl TransformerLayer {
    fn new(config: &ModelConfig, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            self_attn: MultiHeadAttention::new(config, vb.pp("self_attn"))?,
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

/// Multi-head attention
struct MultiHeadAttention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    head_dim: usize,
}

impl MultiHeadAttention {
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
        let attn_weights = candle_nn::ops::softmax(&scores, 1)?;

        // Apply attention to values
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

/// Binary cross-entropy loss
fn binary_cross_entropy(pred: &Tensor, target: &Tensor) -> Result<Tensor> {
    let one = pred.ones_like()?;
    let pred_clamped = pred.clamp(1e-7f32, 1.0 - 1e-7)?;

    let term1 = (target * pred_clamped.log()?)?;
    let term2 = ((&one - target)? * (&one - &pred_clamped)?.log()?)?;
    let loss_sum = (term1 + term2)?.mean_all()?;

    // Negate with scalar multiplication
    let loss = (loss_sum * -1.0)?;

    Ok(loss)
}

/// Sigmoid activation
fn sigmoid(x: &Tensor) -> Result<Tensor> {
    let neg_x = x.neg()?;
    let exp_neg_x = neg_x.exp()?;
    let one_plus_exp = (exp_neg_x + 1.0)?;
    Ok((one_plus_exp.recip())?)
}

/// GELU activation
fn gelu(x: &Tensor) -> Result<Tensor> {
    let sig_input = (x * 1.702)?;
    let sig = sigmoid(&sig_input)?;
    Ok((x * sig)?)
}

impl Saveable for ValidatorModel {
    fn save(&self, path: &Path) -> Result<()> {
        self.varmap.save(path)?;
        Ok(())
    }

    fn load(_path: &Path) -> Result<Self> {
        // TODO: Implement proper loading
        unimplemented!("Validator model loading not yet implemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validator_creation() {
        let config = ModelConfig::default();
        let validator = ValidatorModel::new(&config);
        assert!(validator.is_ok());
    }

    #[test]
    fn test_validator_forward() -> Result<()> {
        let config = ModelConfig::default();
        let validator = ValidatorModel::new(&config)?;

        // Create dummy input (batch_size=1, seq_len=20)
        let input_ids = Tensor::zeros((1, 20), DType::U32, &validator.device)?;

        let output = validator.forward(&input_ids)?;
        assert_eq!(output.dims(), &[1, 1]); // Single probability

        Ok(())
    }

    #[test]
    fn test_validator_validate() -> Result<()> {
        let config = ModelConfig::default();
        let validator = ValidatorModel::new(&config)?;

        // Create dummy query and response
        let query = Tensor::zeros((1, 10), DType::U32, &validator.device)?;
        let response = Tensor::zeros((1, 10), DType::U32, &validator.device)?;

        let result = validator.validate(&query, &response)?;
        // Should return either true or false
        assert!(result == true || result == false);

        Ok(())
    }
}
