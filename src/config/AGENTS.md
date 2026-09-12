# config capsule: configuration, instructions, licensing, metrics

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/config/` (settings, provider entries, personas, credentials and their resolvers,
backend selection, colors, notice state, constants), `src/context/` (project instruction loading),
`src/license/`, `src/metrics/`, `src/monitoring/`, `src/errors.rs`, and the `data/` personas.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/config/mod.rs` is the whole public surface, and
`scripts/check_subsystems.py` rejects a `pub mod` there. `src/context` and `src/license` have their
own module documents and no separate facade yet.

**Dependencies:** layer 0 in [`subsystems.toml`](../../subsystems.toml): `config` should depend on
nothing. It carries debt to `memory`, `models`, and `tools`; importing any other subsystem fails
`scripts/check_subsystems.py`, and the check will not stop a new import of those three, so add none.

**Credentials are secrets.** Resolvers return values that must never reach logs, prompts, metrics,
or error text; see the redaction rules in the root instructions before touching
`credential.rs`.

**Instruction loading is an invariant.** The load order and deduplication rules live in
[context assembly](../context/ASSEMBLY.md) and are pinned by the root
[Context invariant](../../CLAUDE.md#context).

**Focused tests:**
`./scripts/test_brains.sh cargo test --lib -- config:: context:: license:: metrics::`. Run the full
suite when changing a re-exported `pub` item or the on-disk config format, because every subsystem
reads configuration.
