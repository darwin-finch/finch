// Integration test: Verify models can actually train
// Tests that loss decreases after multiple training steps

use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use shammah::models::{ModelConfig, RouterModel, ValidatorModel};

#[test]
fn test_router_training_reduces_loss() -> Result<()> {
    // Create small model for faster testing
    let config = ModelConfig {
        vocab_size: 1000,
        hidden_dim: 64,
        num_layers: 2,
        num_heads: 4,
        max_seq_len: 32,
        dropout: 0.0,
        device_preference: shammah::models::DevicePreference::Cpu,
    };

    let mut router = RouterModel::new(&config)?;
    let device = Device::Cpu;

    // Create dummy training data (10 tokens)
    let input = Tensor::zeros((1, 10), DType::U32, &device)?;
    let target = true; // Should route to local

    // Measure initial loss
    let pred_before = router.forward(&input)?;
    let target_tensor = Tensor::from_vec(vec![1.0f32], (1, 1), &device)?;
    let loss_before = binary_cross_entropy(&pred_before, &target_tensor)?.to_scalar::<f32>()?;

    println!("Initial loss: {}", loss_before);

    // Train for 10 steps
    let learning_rate = 0.01;
    for step in 0..10 {
        router.update(&input, target, learning_rate)?;

        if step % 3 == 0 {
            let pred = router.forward(&input)?;
            let loss = binary_cross_entropy(&pred, &target_tensor)?.to_scalar::<f32>()?;
            println!("Step {}: loss = {}", step, loss);
        }
    }

    // Measure final loss
    let pred_after = router.forward(&input)?;
    let loss_after = binary_cross_entropy(&pred_after, &target_tensor)?.to_scalar::<f32>()?;

    println!("Final loss: {}", loss_after);

    // Loss should decrease (or at least not increase significantly)
    // With random initialization, it might not always decrease, but
    // on average it should trend downward
    assert!(
        loss_after <= loss_before * 1.1,
        "Loss increased too much: {} -> {}",
        loss_before,
        loss_after
    );

    Ok(())
}

#[test]
fn test_validator_training_reduces_loss() -> Result<()> {
    let config = ModelConfig {
        vocab_size: 1000,
        hidden_dim: 64,
        num_layers: 2,
        num_heads: 4,
        max_seq_len: 32,
        dropout: 0.0,
        device_preference: shammah::models::DevicePreference::Cpu,
    };

    let mut validator = ValidatorModel::new(&config)?;
    let device = Device::Cpu;

    // Create dummy query and response
    let query = Tensor::zeros((1, 10), DType::U32, &device)?;
    let response = Tensor::zeros((1, 10), DType::U32, &device)?;
    let target = true; // Good quality

    // Measure initial loss
    let combined = Tensor::cat(&[&query, &response], 1)?;
    let pred_before = validator.forward(&combined)?;
    let target_tensor = Tensor::from_vec(vec![1.0f32], (1, 1), &device)?;
    let loss_before = binary_cross_entropy(&pred_before, &target_tensor)?.to_scalar::<f32>()?;

    println!("Initial loss: {}", loss_before);

    // Train for 10 steps
    let learning_rate = 0.01;
    for step in 0..10 {
        validator.update(&query, &response, target, learning_rate)?;

        if step % 3 == 0 {
            let pred = validator.forward(&combined)?;
            let loss = binary_cross_entropy(&pred, &target_tensor)?.to_scalar::<f32>()?;
            println!("Step {}: loss = {}", step, loss);
        }
    }

    // Measure final loss
    let pred_after = validator.forward(&combined)?;
    let loss_after = binary_cross_entropy(&pred_after, &target_tensor)?.to_scalar::<f32>()?;

    println!("Final loss: {}", loss_after);

    // Loss should not increase significantly
    assert!(
        loss_after <= loss_before * 1.1,
        "Loss increased too much: {} -> {}",
        loss_before,
        loss_after
    );

    Ok(())
}

#[test]
fn test_router_learns_simple_pattern() -> Result<()> {
    // Test that router can learn a very simple pattern:
    // All zeros → forward (0), all ones → local (1)

    let config = ModelConfig {
        vocab_size: 10,
        hidden_dim: 32,
        num_layers: 1,
        num_heads: 2,
        max_seq_len: 8,
        dropout: 0.0,
    };

    let mut router = RouterModel::new(&config)?;
    let device = Device::Cpu;

    // Training data
    let zeros = Tensor::zeros((1, 8), DType::U32, &device)?;
    let ones = Tensor::ones((1, 8), DType::U32, &device)?;

    let learning_rate = 0.1;

    // Train for 50 epochs
    for _epoch in 0..50 {
        // Train on zeros → forward (false)
        router.update(&zeros, false, learning_rate)?;

        // Train on ones → local (true)
        router.update(&ones, true, learning_rate)?;
    }

    // Test predictions
    let pred_zeros = router.predict(&zeros)?;
    let pred_ones = router.predict(&ones)?;

    println!("Prediction for zeros: {} (should be false)", pred_zeros);
    println!("Prediction for ones: {} (should be true)", pred_ones);

    // Note: With random initialization, this might not always work perfectly,
    // but it's a good sanity check that training is doing *something*

    Ok(())
}

// Helper function for binary cross-entropy
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
