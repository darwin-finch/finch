# generators capsule: the unified Generator trait and its implementations

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/generators/`: the `Generator` trait, its request/response/stream types, the
profile-wrapping `ProfiledGenerator`, and the Claude, daemon-local, and Qwen implementations.
DESIGN.md lists this tree on the models row; models owns generators. Provider transports and
OAuth live in `providers` and `oauth`, outside this subtree.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature.
Child modules are private, so the `pub use` list in `src/generators/mod.rs` is the whole public
surface. Callers outside this directory use `crate::generators::Item`; they must not name
`claude`, `daemon_local`, or `qwen`.

**Dependencies:** `claude` (`ContentBlock`, `Message`) and `tools` (`ToolDefinition`).
Implementations may not reach back into `cli`, `models`, or `providers`; callers inject
everything a generator needs. Do not extract `finch-generators` until those edges are measured
and the models-row ownership is a crate-level contract.

**Generator behavior is not a facade commit.** Prompt text, streaming, capability reporting, and
response metadata validation stay as they are. Add public surface by re-exporting it from
`mod.rs`, then regenerate `INTERFACE.md` with `python3 scripts/generate_interfaces.py --write`.
