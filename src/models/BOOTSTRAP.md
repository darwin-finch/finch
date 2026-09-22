# Progressive Bootstrap

*Background: [local backend investigation](../../DESIGN.md#local-backend-investigation) in the design index.*

**Purpose:** Start the REPL while the local model loads in the background.

## GeneratorState machine

| State | Meaning |
|-------|---------|
| `Initializing` | Preparing the configured local chat profile |
| `Downloading` | A managed GGUF is transferring; includes filename and exact byte counts |
| `Loading` | Loading weights into memory |
| `Ready` | Model ready for inference |
| `Failed` | Load failed with error |
| `NotAvailable` | Offline mode |

## Flow

```
1. Daemon starts while the configured model loads in a background task.
2. A custom profile supplies an existing absolute GGUF path, or a managed profile supplies an
   immutable Hugging Face artifact identity.
3. Managed artifacts resume into a partial file, then pass exact-size and SHA-256 checks before
   an atomic rename. The daemon exposes byte progress through `/v1/status`.
4. `UnifiedModelLoader` receives only the verified local path and rejects legacy ONNX/Candle
   entries and unsupported targets.
5. llama.cpp loads the GGUF and creates the shared generator.
6. State becomes `Ready`; the client removes its download status entry and eligible queries can
   use the local model.
```

Startup is not unconditionally instant: launch is gated by a daemon health
probe — `GET /health` under a 500 ms client timeout whose expiry costs an
unconditional two-second sleep in `ensure_daemon_running_after_isolation_gate`
(`src/main.rs`, daemon-connect phase).

While state ≠ `Ready`, `Router::route_with_generator_check(query, false)` forwards all queries to the configured cloud provider API.

## Key files

- `src/models/bootstrap.rs` — `BootstrapLoader`, `GeneratorState`
- `src/models/gguf_download.rs` — managed catalog, resumable transfer, and verification
- `src/models/unified_loader.rs` — config validation and engine selection
- `src/models/loaders/llama_cpp.rs` — GGUF loading and generation

Frontend memory has its own required model defaults and download lifecycle; it does not use this
chat bootstrap path.
