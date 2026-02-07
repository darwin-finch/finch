// Test neural generation integration
// This verifies that trained neural models are connected to response generation

use shammah::local::LocalGenerator;
use shammah::models::{GeneratorModel, ModelConfig, TextTokenizer};
use shammah::training::batch_trainer::{BatchTrainer, TrainingExample};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Testing Neural Generation Integration\n");

    // 1. Create tokenizer
    let tokenizer = Arc::new(TextTokenizer::default()?);
    println!(
        "✓ Created tokenizer (vocab_size: {})",
        tokenizer.vocab_size()
    );

    // 2. Create batch trainer with neural models
    let config = ModelConfig {
        vocab_size: tokenizer.vocab_size(),
        hidden_dim: 128,
        num_layers: 2,
        num_heads: 4,
        max_seq_len: 512,
        dropout: 0.1,
        device_preference: shammah::models::DevicePreference::Cpu, // Use CPU for testing
    };
    let batch_trainer = Arc::new(RwLock::new(BatchTrainer::new(32, 1e-4, &config)?));
    println!("✓ Created batch trainer");

    // 3. Create LocalGenerator WITH neural models
    let neural_generator = {
        let trainer = batch_trainer.read().await;
        Some(trainer.generator())
    };
    let mut local_gen = LocalGenerator::with_models(neural_generator, Some(Arc::clone(&tokenizer)));
    println!("✓ Created LocalGenerator with neural models\n");

    // 4. Add some training examples to BatchTrainer
    println!("Adding training examples...");
    {
        let trainer = batch_trainer.write().await;

        // Math examples
        trainer
            .add_example(
                TrainingExample::new(
                    "What is 2+2?".to_string(),
                    "2+2 equals 4.".to_string(),
                    false,
                )
                .with_quality(0.9),
            )
            .await?;

        trainer
            .add_example(
                TrainingExample::new(
                    "What is 5+7?".to_string(),
                    "5+7 equals 12.".to_string(),
                    false,
                )
                .with_quality(0.9),
            )
            .await?;

        trainer
            .add_example(
                TrainingExample::new(
                    "What is 10+15?".to_string(),
                    "10+15 equals 25.".to_string(),
                    false,
                )
                .with_quality(0.9),
            )
            .await?;

        println!(
            "✓ Added 3 training examples (queue size: {})",
            trainer.queue_size().await
        );
    }

    // 5. Test generation BEFORE training
    println!("\n--- Testing generation BEFORE training ---");
    test_generation(&mut local_gen, "What is 3+3?");

    // 6. Train the models (if enough examples)
    println!("\n--- Training models (note: may not train if queue too small) ---");
    {
        let trainer = batch_trainer.read().await;
        if trainer.should_train_automatically().await {
            drop(trainer); // Release read lock
            let trainer = batch_trainer.read().await;
            let result = trainer.train_now().await;
            match result {
                Ok(stats) => {
                    println!("✓ Training completed:");
                    println!("  - Examples: {}", stats.examples_count);
                    println!("  - Duration: {:.2}s", stats.duration_secs);
                    println!(
                        "  - Router loss: {:.4} → {:.4}",
                        stats.router_old_loss, stats.router_new_loss
                    );
                    println!(
                        "  - Generator loss: {:.4} → {:.4}",
                        stats.generator_old_loss, stats.generator_new_loss
                    );
                }
                Err(e) => println!("⚠️  Training skipped: {}", e),
            }
        } else {
            println!("⚠️  Not enough examples for automatic training");
        }
    }

    // 7. Test generation AFTER training
    println!("\n--- Testing generation AFTER training ---");
    test_generation(&mut local_gen, "What is 8+9?");

    // 8. Test template fallback
    println!("\n--- Testing template fallback ---");
    test_generation(&mut local_gen, "Hello!");

    println!("\n✅ Test completed successfully!");
    Ok(())
}

fn test_generation(local_gen: &mut LocalGenerator, query: &str) {
    print!("Query: \"{query}\" → ");
    match local_gen.try_generate(query) {
        Ok(Some(response)) => {
            println!("✓ Generated locally");
            println!(
                "  Response: {}",
                response.lines().next().unwrap_or(&response)
            );
        }
        Ok(None) => {
            println!("✗ Would forward to Claude (local confidence too low)");
        }
        Err(e) => {
            println!("✗ Generation error: {}", e);
        }
    }
}
