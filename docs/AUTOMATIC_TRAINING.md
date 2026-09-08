# Automatic Training and LoRA (Deferred)

**Status:** Deferred. Automatic training and LoRA adapter loading are not supported Finch
features.

Finch does not automatically collect OpenAI-compatible requests, start a
training timer or worker, invoke Python, rewrite a training queue, generate a
LoRA adapter, or hot-load adapters.

Explicit feedback submitted with Ctrl+G/Ctrl+B, feedback commands, or
`POST /v1/feedback` is stored in `~/.finch/feedback.jsonl`. The store is private
(`~/.finch` mode 0700 and the file mode 0600 on Unix), append-only, locked across
processes, and synced before acknowledgement. Feedback records do not consent
to or trigger training.

Existing `~/.finch/training_queue.jsonl` files and adapter files are preserved
without processing, deletion, or migration. The legacy Python setup command is
an explicit manual experiment and is not connected to the daemon or feedback
path. Finch disabled that background Python path in
[#139 (disable automatic Python LoRA training unless a supported native path is explicitly enabled)](https://github.com/darwin-finch/finch/issues/139).

## Why this work is deferred

The retained code is scaffolding, not a usable training feature. Completing the path currently
requires all of the following unsupported pieces:

- a way to apply adapters in Finch's ONNX inference runtime, tracked in
  [#1 (LoRA adapter loading at ONNX runtime)](https://github.com/darwin-finch/finch/issues/1);
- an external training stack such as MLX on Apple Silicon or PyTorch and PEFT on Linux/CUDA, plus
  adapter conversion and a memory-bounded reload strategy, tracked in
  [#7 (LoRA training memory efficiency)](https://github.com/darwin-finch/finch/issues/7); and
- a verified model/runtime compatibility matrix, tracked in
  [#74 (refresh the local-model catalog and publish a runtime compatibility matrix)](https://github.com/darwin-finch/finch/issues/74).

Finch will use pre-trained artifacts and provider-backed models instead. Training should not be
presented as upcoming or partially available unless the project adopts a supported path with
explicit privacy, resource, cancellation, recovery, and retention controls.
