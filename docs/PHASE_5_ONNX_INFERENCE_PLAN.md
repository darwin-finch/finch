# Phase 5: ONNX Runtime Inference Implementation Plan

**Status**: Planning
**Date**: 2026-02-11
**Goal**: Implement actual ONNX model loading and inference to replace stubs

---

## Current State (Post Phase 4)

### What Works ✅
- Compiles cleanly (0 errors)
- Application runs without crashes
- Graceful degradation to Claude API
- Setup wizard collects model preferences
- Bootstrap loader architecture in place

### What's Stubbed ⚠️
- `OnnxLoader::load_model_sync()` - Downloads model but doesn't create session
- `LoadedOnnxModel::generate()` - Not implemented (no TextGeneration trait)
- `UnifiedModelLoader::load()` - Returns error "Use load_onnx() instead"
- Execution provider selection - Placeholder logic

### Key Files
- `src/models/loaders/onnx.rs` - Main ONNX loader (stub)
- `src/models/loaders/onnx_config.rs` - Model configuration
- `src/models/unified_loader.rs` - Orchestrates loading
- `src/models/bootstrap.rs` - Background loading task
- `src/models/generator_new.rs` - GeneratorModel wrapper

---

## Architecture Overview

```
User Query
    ↓
Router (decides local vs Claude)
    ↓
GeneratorModel (if local)
    ↓
LoadedOnnxModel
    ↓
ort::Session (ONNX Runtime)
    ↓
CoreML EP / CPU EP / CUDA EP
    ↓
Generated Text
```

### Model Loading Flow

```
Setup Wizard
    ↓ (saves config)
~/.shammah/config.toml
    ↓ (read at startup)
REPL::new()
    ↓ (spawns background task)
BootstrapLoader::load_generator_async()
    ↓
UnifiedModelLoader::load_onnx()  ← Need to implement
    ↓
OnnxLoader::load_model_sync()    ← Need to implement
    ↓
LoadedOnnxModel                  ← Need to implement generate()
    ↓
GeneratorModel (wrapper)
    ↓
Ready for inference
```

---

## Implementation Plan

### Step 1: ONNX Model Download ✅ (Mostly Done)

**File**: `src/models/loaders/onnx.rs`

**Current**:
```rust
pub fn load_model_sync(&self, config: &OnnxLoadConfig) -> Result<LoadedOnnxModel> {
    let model_dir = self.download_model(config)?;  // ✅ Works
    let tokenizer = self.load_tokenizer(&model_dir)?;  // ✅ Works

    // TODO: Create ort::Session here

    Ok(LoadedOnnxModel {
        tokenizer,
        model_name: config.model_name.clone(),
        // session: ???
    })
}
```

**Status**: Downloads model files, loads tokenizer, but doesn't create session.

---

### Step 2: ONNX Session Creation (NEW)

**File**: `src/models/loaders/onnx.rs`

**Goal**: Create `ort::Session` from downloaded ONNX files

#### 2.1 Add Session to LoadedOnnxModel

```rust
pub struct LoadedOnnxModel {
    session: ort::Session,  // ← Add this
    tokenizer: Tokenizer,
    model_name: String,
    model_size: ModelSize,
    model_path: PathBuf,
}
```

#### 2.2 Implement Session Creation

```rust
fn create_session(
    &self,
    model_path: &Path,
    config: &OnnxLoadConfig,
) -> Result<ort::Session> {
    // 1. Get execution providers based on config
    let execution_providers = self.get_execution_providers(config)?;

    // 2. Create session with providers
    let session = ort::Session::builder()?
        .with_execution_providers(execution_providers)?
        .with_optimization_level(ort::GraphOptimizationLevel::Level3)?
        .with_intra_threads(4)?  // Parallel ops within layer
        .with_inter_threads(1)?  // Sequential layers
        .commit_from_file(model_path)?;

    // 3. Log which provider was selected
    let provider = session.execution_provider()?;
    tracing::info!("ONNX session created with provider: {:?}", provider);

    Ok(session)
}
```

#### 2.3 Execution Provider Selection

```rust
fn get_execution_providers(
    &self,
    config: &OnnxLoadConfig,
) -> Result<Vec<ort::ExecutionProvider>> {
    let mut providers = vec![];

    match config.backend {
        BackendDevice::CoreML => {
            providers.push(ort::ExecutionProvider::CoreML(Default::default()));
        }
        BackendDevice::Metal => {
            // Metal not directly supported by ort, use CPU
            tracing::warn!("Metal not supported, falling back to CPU");
        }
        BackendDevice::Cuda => {
            #[cfg(feature = "cuda")]
            providers.push(ort::ExecutionProvider::CUDA(Default::default()));
        }
        BackendDevice::Auto => {
            // Try CoreML first on macOS
            #[cfg(target_os = "macos")]
            providers.push(ort::ExecutionProvider::CoreML(Default::default()));

            // Try CUDA on Linux/Windows
            #[cfg(feature = "cuda")]
            providers.push(ort::ExecutionProvider::CUDA(Default::default()));
        }
        _ => {}
    }

    // Always add CPU as fallback
    providers.push(ort::ExecutionProvider::CPU(Default::default()));

    Ok(providers)
}
```

#### 2.4 Update load_model_sync

```rust
pub fn load_model_sync(&self, config: &OnnxLoadConfig) -> Result<LoadedOnnxModel> {
    // 1. Download model (existing, works)
    let model_dir = self.download_model(config)?;

    // 2. Load tokenizer (existing, works)
    let tokenizer = self.load_tokenizer(&model_dir)?;

    // 3. Find ONNX model file
    let model_file = model_dir.join("model.onnx");
    if !model_file.exists() {
        bail!("ONNX model file not found: {:?}", model_file);
    }

    // 4. Create session (NEW)
    let session = self.create_session(&model_file, config)?;

    Ok(LoadedOnnxModel {
        session,  // ← NEW
        tokenizer,
        model_name: config.model_name.clone(),
        model_size: config.size.clone(),
        model_path: model_dir,
    })
}
```

**Testing**:
```bash
cargo test test_onnx_session_creation
```

**Expected**: Session creates successfully, logs execution provider

---

### Step 3: Implement Text Generation (CORE)

**File**: `src/models/loaders/onnx.rs`

**Goal**: Implement autoregressive text generation

#### 3.1 Implement TextGeneration Trait

```rust
use crate::models::generator_new::TextGeneration;

impl TextGeneration for LoadedOnnxModel {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        self.generate_autoregressive(input_ids, max_new_tokens)
    }

    fn name(&self) -> &str {
        &self.model_name
    }
}
```

#### 3.2 Autoregressive Generation

```rust
impl LoadedOnnxModel {
    fn generate_autoregressive(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
    ) -> Result<Vec<u32>> {
        let mut output_ids = input_ids.to_vec();

        // Generation loop
        for _ in 0..max_new_tokens {
            // 1. Prepare input tensor
            let input_tensor = self.prepare_input(&output_ids)?;

            // 2. Run inference
            let outputs = self.session.run(ort::inputs![
                "input_ids" => input_tensor,
            ]?)?;

            // 3. Extract logits
            let logits = self.extract_logits(&outputs)?;

            // 4. Sample next token
            let next_token = self.sample_token(&logits)?;

            // 5. Check for EOS
            if next_token == self.get_eos_token_id() {
                break;
            }

            // 6. Append to output
            output_ids.push(next_token);
        }

        Ok(output_ids)
    }
}
```

#### 3.3 Helper Methods

```rust
impl LoadedOnnxModel {
    fn prepare_input(&self, tokens: &[u32]) -> Result<ort::Value> {
        // Convert tokens to ONNX tensor
        // Shape: [batch_size=1, seq_len]
        let input_shape = [1, tokens.len()];
        let input_data: Vec<i64> = tokens.iter().map(|&t| t as i64).collect();

        ort::Value::from_array(input_shape, &input_data)
    }

    fn extract_logits(&self, outputs: &ort::SessionOutputs) -> Result<Vec<f32>> {
        // Get logits from output tensor
        // Shape typically: [batch_size, seq_len, vocab_size]
        // We want the last token's logits: [vocab_size]

        let output_tensor = outputs.get("logits")
            .or_else(|| outputs.get(0))
            .ok_or_else(|| anyhow::anyhow!("No output tensor"))?;

        let logits: Vec<f32> = output_tensor.try_extract()?;

        // Extract last token's logits
        // TODO: Handle batch dimension and sequence dimension properly

        Ok(logits)
    }

    fn sample_token(&self, logits: &[f32]) -> Result<u32> {
        // Simple greedy sampling (take argmax)
        let max_idx = logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(idx, _)| idx)
            .ok_or_else(|| anyhow::anyhow!("Empty logits"))?;

        Ok(max_idx as u32)
    }

    fn get_eos_token_id(&self) -> u32 {
        // Get from tokenizer config
        // For Qwen: typically 151643
        // For now, hardcode common value
        151643
    }
}
```

**Testing**:
```rust
#[test]
fn test_simple_generation() {
    let model = load_test_model();
    let input = "Hello, my name is";
    let input_ids = model.tokenizer.encode(input, true)?;
    let output_ids = model.generate(&input_ids, 10)?;
    let output = model.tokenizer.decode(&output_ids, true)?;

    assert!(!output.is_empty());
    println!("Generated: {}", output);
}
```

---

### Step 4: Wire into UnifiedModelLoader

**File**: `src/models/unified_loader.rs`

#### 4.1 Replace load() with load_onnx()

```rust
pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
    // Phase 5: Call load_onnx() instead of returning error
    let onnx_model = self.load_onnx(config)?;
    Ok(Box::new(onnx_model))
}

pub fn load_onnx(&self, config: ModelLoadConfig) -> Result<LoadedOnnxModel> {
    // Convert ModelLoadConfig to OnnxLoadConfig
    let onnx_config = OnnxLoadConfig {
        model_name: self.get_model_name(&config),
        size: config.size,
        cache_dir: self.cache_dir.clone(),
        backend: config.backend,
    };

    // Load via ONNX loader
    let loader = OnnxLoader::new(self.cache_dir.clone());
    loader.load_model_sync(&onnx_config)
}

fn get_model_name(&self, config: &ModelLoadConfig) -> String {
    if let Some(ref repo) = config.repo_override {
        return repo.clone();
    }

    match config.family {
        ModelFamily::Qwen2 => format!(
            "Qwen/Qwen2.5-{}-Instruct",
            config.size.to_size_string(config.family)
        ),
        // ... other families
    }
}
```

**Testing**:
```bash
cargo test test_unified_loader_onnx
```

---

### Step 5: Test End-to-End

#### 5.1 Unit Tests

```bash
cargo test loaders::onnx
cargo test unified_loader
```

#### 5.2 Integration Test

```rust
#[tokio::test]
async fn test_bootstrap_loader_onnx() {
    let state = Arc::new(RwLock::new(GeneratorState::Initializing));
    let loader = BootstrapLoader::new(state.clone(), None);

    loader
        .load_generator_async(
            ModelFamily::Qwen2,
            ModelSize::Small,
            BackendDevice::CoreML,
            None,
        )
        .await
        .unwrap();

    let final_state = state.read().await;
    match &*final_state {
        GeneratorState::Ready { model, .. } => {
            // Test generation
            let mut gen = model.write().await;
            let response = gen.generate_text("Hello", 10).unwrap();
            assert!(!response.is_empty());
        }
        other => panic!("Expected Ready, got {:?}", other),
    }
}
```

#### 5.3 Manual Testing

```bash
# 1. Clean build
cargo clean && cargo build

# 2. Run interactive
./target/debug/shammah

# Expected output:
# ⏳ Initializing Qwen model (background)...
# ⏳ Loading Qwen2.5-1.5B-Instruct...
# ✓ ONNX session created with provider: CoreML
# ✓ Model ready for inference

# 3. Test query
> Tell me about Rust

# Expected: Local generation (not Claude API)
```

---

## Challenges & Solutions

### Challenge 1: Model Format Compatibility

**Problem**: ONNX models from different sources may have different input/output names

**Solution**:
- Query model metadata to discover input/output names
- Support multiple naming conventions (input_ids, input, x)
- Add model-specific adapters if needed

### Challenge 2: KV Cache

**Problem**: Naive generation is slow (recomputes all tokens each step)

**Solution**: Phase 5.1 - Implement in Phase 6
- For now, accept slower generation
- Future: Implement KV cache support

### Challenge 3: Execution Provider Selection

**Problem**: CoreML may not be available, need graceful fallback

**Solution**:
- Try requested provider first
- Fall back to CPU automatically
- Log which provider is actually used
- Update UI to show active provider

### Challenge 4: Model Size vs RAM

**Problem**: User might select model too large for system

**Solution**:
- Check available RAM before loading
- Recommend smaller model if needed
- Show memory usage in status bar

---

## Success Criteria

✅ **Compilation**: Builds without errors
✅ **Session Creation**: Creates ort::Session successfully
✅ **Execution Provider**: CoreML/CPU provider active
✅ **Model Loading**: Downloads and loads model from HuggingFace
✅ **Text Generation**: Generates coherent text (not gibberish)
✅ **Setup Wizard Integration**: Uses config.model_family/size/backend
✅ **Graceful Degradation**: Falls back to Claude if ONNX fails
✅ **Performance**: Generates at reasonable speed (>1 token/sec on CPU)

---

## Testing Strategy

### Unit Tests
- Session creation with different execution providers
- Tokenization (encode/decode)
- Tensor preparation
- Logits extraction
- Token sampling

### Integration Tests
- Full generation pipeline
- Bootstrap loader integration
- Config → ONNX loader flow

### Manual Testing
- Run on macOS with CoreML
- Test different model sizes (Small/Medium/Large)
- Test with custom model repo
- Verify graceful degradation

---

## Rollback Plan

If ONNX implementation fails:

```bash
# Rollback to Phase 4 checkpoint
git reset --hard HEAD~1  # Undo ONNX implementation commit

# Or create rollback branch
git checkout -b phase-4-stable
git reset --hard 00b08cf  # Phase 4 completion commit
```

All queries will forward to Claude API (same as current behavior).

---

## Timeline Estimate

- **Step 1**: Already done ✅
- **Step 2**: ONNX session creation - 1-2 hours
- **Step 3**: Text generation - 2-3 hours (most complex)
- **Step 4**: Wire into loader - 30 minutes
- **Step 5**: Testing - 1 hour

**Total**: ~5-7 hours of focused work

---

## Next Steps After Phase 5

**Phase 6**: Performance optimization
- Implement KV cache
- Batch inference
- Streaming generation

**Phase 7**: LoRA adapter support
- Python training scripts
- Adapter loading in ONNX
- Multiple adapter management

---

## Questions to Resolve

1. **Model format**: Which ONNX repos to target?
   - `onnx-community/Qwen2.5-*-Instruct`?
   - Custom converted models?

2. **Input/output names**: Standardize or auto-detect?
   - Query model metadata?
   - Use model-specific configs?

3. **Sampling strategy**: Start with greedy or add temperature/top-k?
   - Greedy for MVP
   - Add sampling later

4. **Error handling**: Retry logic for session creation?
   - Single attempt + fallback to Claude
   - Don't block user with retries

---

**Ready to begin implementation when you are!**
