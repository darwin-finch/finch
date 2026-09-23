# Local-model support

`models` owns the optional local backend's loader state, model-family identity, adapters,
download/bootstrap machinery, and engine-facing capability claims. It does not own request
routing, conversation policy, tool execution, or terminal presentation. A configured family
and a loader implementation do not establish that a model has loaded or passed end-to-end
conformance.

Local chat uses llama.cpp. In `finch setup`, choose a supported Qwen or Gemma size and
quantization and leave the file field blank to select a Finch-managed GGUF. The daemon downloads
the exact commit-pinned Hugging Face artifact into Finch's cache, verifies its size and SHA-256,
then passes only the verified local path into `UnifiedModelLoader`. An existing absolute `.gguf`
path remains available for custom models. Finch's configured Hugging Face token is injected by the
daemon; otherwise the standard hf-hub environment/cache token is used. The user-configured local model setting is for chat
LLMs, not memory embedders or rerankers.
For GGUF, `execution_target = "auto"` permits GPU offload when available;
`execution_target = "cpu"` disables it. Removed CoreML and CUDA chat target names are rejected
rather than silently remapped.

Memory embeddings are outside this local chat-provider slice. The frontend memory selector,
defaults, and automatic model download remain independently owned. Its current ONNX dependency
does not make ONNX a daemon chat provider.

Two callers show the boundary:

1. The [daemon startup path](../../src/main.rs) injects a host-owned `ModelProgress` sink,
   creates shared `GeneratorState`, then starts `BootstrapLoader::load_generator_async` only
   when the local backend is enabled. Managed download bytes are published in that state; the
   interactive client polls `/v1/status`, updates one status-bar entry, and removes it at every
   terminal outcome. A failed load becomes a failed state and the daemon can forward requests
   to cloud provider APIs; proxy-only mode marks the model unavailable.
2. The [local response generator](../local/generator.rs) selects an adapter through
   `AdapterRegistry` for its configured model family and may receive a shared `GeneratorModel`
   handle from the application. It uses the adapter for prompt/output handling; it does not
   create or load that handle. The REPL currently constructs a compatibility bootstrap state
   without loading a model locally; the daemon owns the actual load.

Read [AGENTS.md](AGENTS.md) for the dependency and lifecycle contract, and [`mod.rs`](mod.rs)
for the flat callable facade. [BOOTSTRAP.md](BOOTSTRAP.md) covers loader lifecycle details.
