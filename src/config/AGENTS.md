# config capsule: configuration, instructions, licensing, metrics

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/config/` (settings, provider entries, personas, credentials and their resolvers,
backend selection, colors, notice state, constants), `src/context/` (project instruction loading),
`src/license/`, `src/metrics/`, `src/monitoring/`, `src/errors.rs`, and the `data/` personas.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/config/mod.rs` is the whole public surface, and
`scripts/check_subsystems.py` rejects a `pub mod` there. `src/context` and `src/license` have their
own module documents and no separate facade yet.

**Dependencies:** `config` should depend on nothing except the extracted provider
credential/reasoning types from `finch-providers`. Three unwanted edges remain, to
`memory`, `models`, and `tools::mcp` (`McpServerConfig`). Add no new ones.

**Credentials are secrets.** Resolvers return values that must never reach logs, prompts, metrics,
or error text; see the redaction rules in the root instructions before touching
`credential.rs`.

**Startup writes nothing; saves are atomic.** Ordinary start and attach perform no configuration
save — notice bookkeeping lives in `notice_state.toml`, and the legacy `notice_suppress_until` is
read but never written back (#76). Only intentional changes save, and every save goes through
`atomic_write` in this module: symlink target refused, private same-directory temporary, fsync,
mode preserved, atomic rename, temporary removed on failure. The production-boundary regressions
that drive the real binary are `tests/startup_is_readonly_on_config.rs`.

**Instruction loading is an invariant.** The load order and deduplication rules live in
[context assembly](../context/ASSEMBLY.md) and are pinned by the root
[Context invariant](../../CLAUDE.md#context).

**Focused tests:**
`./scripts/test_brains.sh cargo test --lib -- config:: context:: license:: metrics::`. Run the full
suite when changing a re-exported `pub` item or the on-disk config format, because every subsystem
reads configuration.
