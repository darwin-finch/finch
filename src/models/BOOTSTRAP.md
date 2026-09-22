# Progressive Bootstrap

*Background: [local backend investigation](../../DESIGN.md#local-backend-investigation) in the design index.*

**Purpose:** Start the REPL while the local model loads in the background.

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
1. REPL starts while the model loads in the background
2. tokio::spawn background task
3. Check cache (~/.cache/huggingface/)
4. Download if needed (progress bar)
5. Load model weights
6. state → Ready
7. Future queries use local model
```

Startup is not unconditionally instant: launch is gated by a daemon health
probe — `GET /health` under a 500 ms client timeout whose expiry costs an
unconditional two-second sleep in `ensure_daemon_running_after_isolation_gate`
(`src/main.rs`, daemon-connect phase).

While state ≠ `Ready`, `Router::route_with_generator_check(query, false)` forwards all queries to the configured cloud provider API.

## Key files

- `src/models/bootstrap.rs` — `BootstrapLoader`, `GeneratorState`
- `src/models/download.rs` — `ModelDownloader` with HF Hub integration
- `src/models/model_selector.rs` — RAM-based model selection
