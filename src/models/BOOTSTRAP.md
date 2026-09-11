# Progressive Bootstrap

*Background: [local backend investigation](../../DESIGN.md#local-backend-investigation) in the design index.*

**Purpose:** Instant startup with background model loading.

## GeneratorState machine

| State | Meaning |
|-------|---------|
| `Initializing` | Selecting model based on RAM |
| `Downloading` | Fetching from HuggingFace Hub (first run) |
| `Loading` | Loading weights into memory |
| `Ready` | Model ready for inference |
| `Failed` | Load failed with error |
| `NotAvailable` | Offline mode |

## Flow

```
1. REPL appears instantly (<100ms)
2. tokio::spawn background task
3. Check cache (~/.cache/huggingface/)
4. Download if needed (progress bar)
5. Load model weights
6. state → Ready
7. Future queries use local model
```

While state ≠ `Ready`, `Router::route_with_generator_check(query, false)` forwards all queries to the configured teacher API.

## Key files

- `src/models/bootstrap.rs` — `BootstrapLoader`, `GeneratorState`
- `src/models/download.rs` — `ModelDownloader` with HF Hub integration
- `src/models/model_selector.rs` — RAM-based model selection
