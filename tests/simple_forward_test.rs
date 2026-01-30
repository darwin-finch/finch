// Simple forward pass test without training

use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use shammah::models::{ModelConfig, RouterModel};

#[test]
fn test_simple_forward_pass() -> Result<()> {
    println!("Creating model...");
    let config = ModelConfig {
        vocab_size: 100,
        hidden_dim: 32,
        num_layers: 1,
        num_heads: 2,
        max_seq_len: 16,
        dropout: 0.0,
    };

    let router = RouterModel::new(&config)?;
    println!("Model created successfully!");

    let device = Device::Cpu;
    let input = Tensor::zeros((1, 8), DType::U32, &device)?;
    println!("Input shape: {:?}", input.dims());

    println!("Running forward pass...");
    let output = router.forward(&input)?;
    println!("Output shape: {:?}", output.dims());

    // Squeeze to get scalar
    let prob_squeezed = output.squeeze(0)?.squeeze(0)?;
    let prob = prob_squeezed.to_scalar::<f32>()?;
    println!("Output value: {:?}", prob);

    // Just make sure we get a probability between 0 and 1
    assert!(
        prob >= 0.0 && prob <= 1.0,
        "Probability out of range: {}",
        prob
    );

    println!("Forward pass successful!");
    Ok(())
}

#[test]
fn test_predict() -> Result<()> {
    let config = ModelConfig {
        vocab_size: 100,
        hidden_dim: 32,
        num_layers: 1,
        num_heads: 2,
        max_seq_len: 16,
        dropout: 0.0,
    };

    let router = RouterModel::new(&config)?;
    let device = Device::Cpu;
    let input = Tensor::zeros((1, 8), DType::U32, &device)?;

    println!("Testing predict method...");
    let decision = router.predict(&input)?;
    println!("Decision: {}", decision);

    Ok(())
}
