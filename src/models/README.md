# models: local model loading, routing, and training configuration

Owns ONNX/Candle loaders, bootstrap and download, adapters, sampling, threshold routing, LoRA
configuration, and neural embedding load. It exists as the one place local-model concerns live so
the rest of Finch (CLI, Brain, tools) never has to know whether a given turn is served locally or
by a cloud provider — that decision and its loading mechanics are entirely behind this facade.
Configuration variants and loader code here are not proof of end-to-end conformance; see the root
[provider and local-model claims](../../CLAUDE.md#provider-and-local-model-claims) policy.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

- [`ONNX.md`](ONNX.md) — the ONNX loader. Flagged in `DESIGN.md` as stating unmeasured startup
  timing; read with that caveat.
- [`BOOTSTRAP.md`](BOOTSTRAP.md) — bootstrap loading. Same caveat as above.
- [`LORA.md`](LORA.md) — the deferred LoRA adapter path (automatic training is disabled).
- [`../router/ROUTING.md`](../router/ROUTING.md) — the routing decision this subsystem feeds.
- [`../../docs/AUTOMATIC_TRAINING.md`](../../docs/AUTOMATIC_TRAINING.md) — automatic-training
  status (deferred).
