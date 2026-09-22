# Local-model support

`models` owns the optional local backend's loader state, model-family identity, adapters,
download/bootstrap machinery, and engine-facing capability claims. It does not own request
routing, conversation policy, tool execution, or terminal presentation. A configured family
and a loader implementation do not establish that a model has loaded or passed end-to-end
conformance.

Two callers show the boundary:

1. The [daemon startup path](../../src/main.rs) installs a host-owned `ModelProgress` sink,
   creates shared `GeneratorState`, then starts `BootstrapLoader::load_generator_async` only
   when the local backend is enabled. A failed load becomes a failed state and the daemon
   can forward requests to cloud provider APIs; proxy-only mode marks the model unavailable.
   `src/models` reports progress and state, while daemon startup owns the process and fallback.
2. The [local response generator](../local/generator.rs) selects an adapter through
   `AdapterRegistry` for its configured model family and may receive a shared `GeneratorModel`
   handle from the application. It uses the adapter for prompt/output handling; it does not
   create or load that handle. The REPL currently constructs a compatibility bootstrap state
   without loading a model locally; the daemon owns the actual load.

Read [AGENTS.md](AGENTS.md) for the dependency and lifecycle contract, and [`mod.rs`](mod.rs)
for the flat callable facade. [BOOTSTRAP.md](BOOTSTRAP.md) and [ONNX.md](ONNX.md) cover loader
details; neither is a support or quality claim.
