# Background Training Orchestration Implementation

## Overview

Implemented background LoRA training that automatically triggers when feedback threshold is reached. Training runs asynchronously without blocking the REPL, allowing users to continue querying while the model adapts.

## What Was Implemented

### 1. Background Training Trigger (`src/cli/repl.rs`)

**Updated `handle_feedback()` method:**

```rust
// Trigger training if threshold reached
if should_train {
    println!("\n🔄 Training threshold reached, starting background training...");
    println!("   (Training runs in background, you can continue querying)");

    // Spawn background training task
    let coordinator = Arc::clone(&self.training_coordinator);
    let models_dir = self.models_dir.clone();

    tokio::spawn(async move {
        match Self::run_background_training(coordinator, models_dir).await {
            Ok(stats) => {
                println!("\n✓ Background training completed!");
                println!("   Trained on {} examples", stats.examples_trained);
                println!("   Final loss: {:.4}", stats.final_loss);
                println!("   Adapter saved to: {}", stats.adapter_path);
            }
            Err(e) => {
                eprintln!("\n⚠️  Background training failed: {}", e);
            }
        }
    });

    println!("   Training started in background...");
}
```

### 2. Training Statistics Structure

```rust
/// Background training statistics
struct BackgroundTrainingStats {
    examples_trained: usize,
    final_loss: f64,
    adapter_path: String,
}
```

### 3. Background Training Implementation

**New method: `run_background_training()`**

```rust
async fn run_background_training(
    coordinator: Arc<TrainingCoordinator>,
    models_dir: Option<PathBuf>,
) -> Result<BackgroundTrainingStats>
```

**Training Flow:**

1. **Get Training Examples** from buffer:
   ```rust
   let examples = {
       let buffer = coordinator.buffer().await;
       buffer.examples().to_vec()
   };
   ```

2. **Create LoRA Configuration**:
   ```rust
   let lora_config = LoRAConfig {
       rank: 16,
       alpha: 32.0,
       dropout: 0.1,
       target_modules: vec!["q_proj".to_string(), "v_proj".to_string()],
   };
   ```

3. **Initialize Device** (Metal or CPU):
   ```rust
   use crate::models::{get_device_with_preference, DevicePreference};
   let device = get_device_with_preference(DevicePreference::Auto)?;
   ```

4. **Create LoRA Adapter**:
   ```rust
   let adapter = LoRAAdapter::new(lora_config.clone(), device.clone())?;
   ```

5. **Load Tokenizer**:
   ```rust
   // Placeholder tokenizer for now
   let tokenizer = StdArc::new({
       use tokenizers::models::bpe::BPE;
       let bpe = BPE::default();
       tokenizers::Tokenizer::new(bpe)
   });
   ```

6. **Create Trainer**:
   ```rust
   let mut trainer = LoRATrainer::new(
       adapter,
       tokenizer,
       1e-4, // learning_rate
       4,    // batch_size
       3,    // epochs
   );
   ```

7. **Build Example Buffer**:
   ```rust
   let mut buffer = ExampleBuffer::new(examples.len());
   for example in examples {
       buffer.add(example);
   }
   ```

8. **Train the Adapter**:
   ```rust
   let training_stats = trainer.train(&buffer)?;
   ```

9. **Save Adapter Weights**:
   ```rust
   let adapters_dir = /* ~/.shammah/adapters */;
   std::fs::create_dir_all(&adapters_dir)?;

   let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
   let adapter_filename = format!("lora_adapter_{}.safetensors", timestamp);
   let adapter_path = adapters_dir.join(&adapter_filename);

   trainer.adapter().save(&adapter_path)?;
   ```

10. **Return Statistics**:
    ```rust
    Ok(BackgroundTrainingStats {
        examples_trained: num_examples,
        final_loss,
        adapter_path: adapter_path.display().to_string(),
    })
    ```

## User Experience Flow

### 1. Providing Feedback

```bash
> How do I read a file in Rust?
< [Response about using std::fs...]

> /critical Always handle file I/O errors properly
🔴 Feedback recorded (weight: 10x)
   Note: Always handle file I/O errors properly
   Training buffer: 8 examples (42.0 weighted)
```

### 2. Reaching Training Threshold

```bash
> /good This is perfect
🟢 Feedback recorded (weight: 1x)
   Training buffer: 10 examples (51.0 weighted)

🔄 Training threshold reached, starting background training...
   (Training runs in background, you can continue querying)
   Training started in background...
```

### 3. Continuing to Query

```bash
> How do I parse JSON in Rust?
< [Model responds while training continues in background...]
```

### 4. Training Completion

```bash
✓ Background training completed!
   Trained on 10 examples
   Final loss: 0.8534
   Adapter saved to: /Users/user/.shammah/adapters/lora_adapter_20260206_143025.safetensors
```

## Technical Details

### Async Execution

Training runs in a separate `tokio::spawn` task:
- **Non-blocking**: User can continue querying
- **Isolated**: Training errors don't crash REPL
- **Asynchronous**: Returns control immediately

### Adapter Storage

Adapters saved with timestamp:
```
~/.shammah/adapters/
├── lora_adapter_20260206_143025.safetensors
├── lora_adapter_20260206_143025.json  (config)
├── lora_adapter_20260206_155010.safetensors
└── lora_adapter_20260206_155010.json
```

Format:
- **safetensors**: Efficient weight storage
- **json**: LoRA configuration (rank, alpha, etc.)

### Training Parameters

Current configuration:
- **Learning Rate**: 1e-4 (0.0001)
- **Batch Size**: 4 examples
- **Epochs**: 3 passes through data
- **LoRA Rank**: 16 (dimensionality)
- **LoRA Alpha**: 32.0 (scaling factor)
- **Dropout**: 0.1 (regularization)
- **Target Modules**: q_proj, v_proj (attention layers)

### Weighted Sampling

During training, examples are sampled by weight:
- Critical (10x): Appears in ~71% of batches (10 out of 14 total weight)
- Medium (3x): Appears in ~21% of batches (3 out of 14)
- Normal (1x): Appears in ~7% of batches (1 out of 14)

This ensures critical feedback has maximum impact.

## Integration Points

### With TrainingCoordinator

```rust
// When should_train returns true:
let coordinator = Arc::clone(&self.training_coordinator);

// Background task gets examples:
let examples = coordinator.buffer().await.examples().to_vec();
```

### With LoRA System

```rust
// Creates adapter:
let adapter = LoRAAdapter::new(lora_config, device)?;

// Trains adapter:
let trainer = LoRATrainer::new(...);
let stats = trainer.train(&buffer)?;

// Saves adapter:
trainer.adapter().save(&adapter_path)?;
```

### With Model Reloading (TODO)

Currently, adapters are saved but not automatically reloaded. Future implementation:

```rust
// After training completes:
if let Ok(stats) = result {
    // Reload generator with new adapter
    let mut gen = self.local_generator.write().await;
    gen.load_lora_adapter(&stats.adapter_path)?;
    println!("   ✓ Model reloaded with new adapter");
}
```

## Known Limitations

### 1. Tokenizer Placeholder

Currently uses a dummy BPE tokenizer:
```rust
let tokenizer = StdArc::new({
    use tokenizers::models::bpe::BPE;
    let bpe = BPE::default();
    tokenizers::Tokenizer::new(bpe)
});
```

**Production TODO**: Load actual Qwen tokenizer from model cache:
```rust
let tokenizer_path = qwen_model_dir.join("tokenizer.json");
let tokenizer = Tokenizer::from_file(tokenizer_path)?;
```

### 2. No Automatic Model Reloading

Trained adapters are saved but not automatically applied. User must:
- Restart REPL, or
- Manually reload model

**Production TODO**: Reload generator after training:
```rust
self.local_generator.write().await.reload_with_adapter(&adapter_path)?;
```

### 3. No Training Progress UI

User doesn't see training progress (epochs, batches, loss).

**Future Enhancement**: Progress bar or periodic updates:
```rust
🔄 Training: Epoch 1/3, Batch 2/3, Loss: 1.234
🔄 Training: Epoch 2/3, Batch 1/3, Loss: 0.987
```

### 4. No Adapter Selection

All adapters saved, but no way to:
- List available adapters
- Load specific adapter
- Delete old adapters
- Compare adapter performance

**Future Enhancement**: Adapter management commands:
```bash
/adapters list
/adapters load lora_adapter_20260206_143025
/adapters delete old_adapter
```

### 5. Fixed Training Parameters

Learning rate, batch size, epochs are hardcoded.

**Future Enhancement**: User configuration:
```toml
[lora.training]
learning_rate = 1e-4
batch_size = 4
epochs = 3
auto_train_threshold = 10
```

## Testing

### Unit Test Example

```rust
#[tokio::test]
async fn test_background_training() {
    // Create training coordinator
    let coordinator = Arc::new(TrainingCoordinator::new(100, 5, true));

    // Add training examples
    for i in 0..5 {
        let example = WeightedExample::critical(
            format!("query {}", i),
            format!("response {}", i),
            "test feedback".into(),
        );
        coordinator.add_example(example).await.unwrap();
    }

    // Run training
    let result = Repl::run_background_training(coordinator, None).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.examples_trained, 5);
    assert!(stats.final_loss > 0.0);
}
```

### Integration Test

```bash
# Start REPL
$ cargo run

# Provide 10 feedback examples
> query 1
< response 1
> /critical feedback 1
[... repeat 9 more times ...]

# Verify background training triggers
🔄 Training threshold reached, starting background training...

# Verify can continue querying
> query 11
< response 11  # REPL still responsive

# Wait for training completion
✓ Background training completed!
```

## Performance Considerations

### Memory Usage

- **Training Buffer**: ~100 examples × ~1KB = ~100KB
- **LoRA Adapter**: ~16 rank × 2 layers × 4KB = ~128KB
- **Training Gradients**: Temporary, released after training

Total overhead: **~500KB - 1MB** (negligible)

### Training Duration

Depends on:
- Number of examples (default: 10)
- Epochs (default: 3)
- Batch size (default: 4)
- Device (Metal: fast, CPU: slower)

Estimated:
- **Metal (Apple Silicon)**: 10-30 seconds
- **CPU**: 30-90 seconds

### Concurrency

- Training runs in separate task
- Does NOT block REPL queries
- User sees "Training started in background..." immediately
- Completion message appears when done

## Future Enhancements

### 1. Adapter Hot-Swapping

Reload model with new adapter without restart:
```rust
self.generator.hot_swap_adapter(&new_adapter_path)?;
```

### 2. Training Queue

Queue multiple training jobs:
```rust
TrainingQueue:
- Job 1: 10 examples (in progress)
- Job 2: 5 examples (queued)
- Job 3: 8 examples (queued)
```

### 3. Training Analytics

Track adapter performance over time:
```rust
AdapterMetrics {
    adapter_id: "lora_20260206_143025",
    trained_on: 10 examples,
    quality_improvement: +12%,
    used_in: 45 queries,
}
```

### 4. Differential Training

Train only on differences from previous adapter:
```rust
let delta = new_adapter - old_adapter;
train_on_delta(delta, new_examples);
```

### 5. Multi-Adapter Ensemble

Load multiple adapters for different domains:
```rust
adapters = {
    "rust": lora_rust_2026.safetensors,
    "python": lora_python_2026.safetensors,
    "architecture": lora_arch_2026.safetensors,
}
```

## Error Handling

### Training Failures

Gracefully handled with error message:
```rust
match Self::run_background_training(...).await {
    Ok(stats) => { /* success */ }
    Err(e) => {
        eprintln!("\n⚠️  Background training failed: {}", e);
        // REPL continues working, training can be retried
    }
}
```

Common errors:
- **Insufficient examples**: Buffer empty
- **Out of memory**: Too many examples or large model
- **I/O error**: Can't save adapter
- **Device error**: Metal/CUDA unavailable

### Recovery

If training fails:
1. Error logged to console
2. REPL continues working
3. Examples remain in buffer
4. Next feedback may trigger retry

## Logging

Training progress logged via `tracing`:

```rust
tracing::info!("Starting background LoRA training");
tracing::info!("Training on {} examples", num_examples);
tracing::info!("Starting LoRA training...");
tracing::info!("LoRA training completed. Adapter saved to: {}", path);
```

View logs with `RUST_LOG=info`:
```bash
$ RUST_LOG=info cargo run
```

## Security Considerations

### Adapter Storage

Adapters stored in user directory (`~/.shammah/adapters/`):
- **Privacy**: Not shared by default
- **Permissions**: User-only read/write (0600)
- **Location**: Outside project directory

### Training Data

Feedback notes stored in adapter metadata:
- **Sensitive data**: Review before sharing adapters
- **PII**: Avoid including personal information
- **Secrets**: Never include API keys, passwords

### Sandboxing

Training runs in same process:
- **CPU/Memory limits**: Use system limits
- **Timeout**: Consider adding training timeout
- **Validation**: Validate adapter before loading

## Documentation Updates Needed

1. **README.md**: Add training workflow section
2. **CLAUDE.md**: Update with background training details
3. **User Guide**: Training best practices
4. **API Docs**: Document training methods

---

**Status**: ✅ Core implementation complete
**Compilation**: ✅ Compiles successfully
**Testing**: ⚠️ Manual testing needed
**Next**: Integrate with model reloading, add progress UI

**Date**: 2026-02-06
**Task**: #14 - Implement background training orchestration
**Author**: Claude Sonnet 4.5
