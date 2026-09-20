# generators: Finch adapters over the generation contract

Owns the Finch-facing adapters (`ClaudeGenerator`, `DaemonLocalGenerator`, `QwenGenerator`,
`ProfiledGenerator`) that implement the generation contract defined in
[`finch-generation`](../../crates/finch-generation/README.md). It exists as the seam between that
provider-neutral crate and Finch's actual generator choices — the REPL and scheduler depend on
this module's `Generator` trait, never on `finch-generation` or a specific provider directly.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
