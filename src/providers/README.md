# Finch provider composition

This module is the root application's compatibility facade over `finch-providers`. It owns
the mapping from Finch `Config`, provider entries, and credential resolution to concrete
provider profiles and graphs. The extracted crate owns HTTP transports, OAuth dialects,
validated requests, catalogs, and provider-neutral wire types. This module does not own
Brain, TUI, daemon, or tool execution policy.

At application startup, `src/main.rs` calls `create_provider_graph_from_config` and passes
the selected graph to the Claude compatibility client or daemon composition. The factory
maps application profile and credential choices to the crate's provider implementations;
it must not bypass the crate's validated dispatch boundary.

At interactive REPL startup, `src/cli/repl.rs` rebuilds a selected provider profile with
`create_provider_profile_from_config`, wraps it in `ClaudeGenerator`, and retains the profile
name for display. The REPL owns model/profile selection; this module owns the Config-to-provider
construction step. The duplicate graph construction is a measured startup cost, not a
separate provider contract.

Read [AGENTS.md](AGENTS.md) for dependency rules, [mod.rs](mod.rs) for flat exports, and
root-package rustdoc for callable methods. For transport or catalog changes, use the
[`finch-providers` capsule](../../crates/finch-providers/README.md).
