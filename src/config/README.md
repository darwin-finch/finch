# Finch application configuration

This module reads and validates Finch settings, provider entries, personas, backend choices,
and credential references, and atomically saves intentional configuration edits. It keeps
licence-notice bookkeeping in separate runtime state. It is an application configuration
boundary, not a provider transport or model loader. Credential resolution contracts are
re-exported from `finch-providers`; the root package decides how saved choices compose a run.
Project instruction files belong to `src/context`, not this module.

At startup, `src/main.rs` calls `load_config` before constructing provider graphs, daemon
services, or a query session. Normal startup reads the saved configuration but does not save it.
Intentional edits use `Config::save` and the module's atomic-write path; notice bookkeeping is
kept in separate runtime state so merely starting Finch does not rewrite `config.toml`.

For provider construction, `src/providers/factory.rs` reads `Config` provider entries and
credential bindings and maps them onto `finch-providers` implementations. This module owns the
persisted application vocabulary and secret-bearing resolver contracts; the provider factory
owns profile selection, and the extracted crate owns validated transport dispatch.

Read [AGENTS.md](AGENTS.md) for secret, lifetime, and testing rules,
[mod.rs](mod.rs) for the flat facade, and root-package rustdoc for methods. The on-disk shape
and migration limits live in [CONFIGURATION.md](CONFIGURATION.md).
