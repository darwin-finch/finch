# config capsule: configuration, instructions, licensing, metrics

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/config/` (settings, provider entries, personas, credential bindings and resolver
re-exports, backend selection, persisted color choices, notice state, constants). The shared color
vocabulary itself belongs to `finch-theme`.
The `data/` personas are configuration inputs. `src/context/` has its own capsule; license,
metrics, monitoring, and errors are other root-map areas, not files owned by `src/config/`.
The declared post-edit diagnostics sources live in
[`diagnostics.rs`](diagnostics.rs) and their user-facing contract is documented in
[`CONFIGURATION.md`](CONFIGURATION.md).

**Boundary:** the [README](README.md) traces startup and provider-factory callers.
[`mod.rs`](mod.rs) is the flat facade; rustdoc renders methods on exported types. Child modules
are private. `scripts/check_subsystems.py` does not exist; use
`python3 scripts/check_facade_boundaries.py` for the current facade check. Do not recreate a
signature catalog.

**Dependencies:** `config` may use the extracted provider credential/reasoning types from
`finch-providers` and the persisted color vocabulary from `finch-theme`. Three unwanted edges remain, to
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

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch --lib -- config::` for this
module, and a separate `./scripts/test_brains.sh cargo test -p finch --lib -- context::` for
instruction/mention behavior. Run the full suite when changing a re-exported `pub` item or the
on-disk config format, because every subsystem reads configuration.
