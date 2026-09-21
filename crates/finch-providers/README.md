# Finch provider transports

This crate owns provider-neutral request and stream contracts plus the concrete Claude,
OpenAI-compatible, Gemini, ChatGPT, and Grok transports, provider capability and model-catalog
logic, credential ports, and OAuth state machines. It validates provider-specific wire behavior
without importing Finch application configuration. The root application maps `Config` onto these
contracts and decides which provider profile to run; generation lifecycle and tool execution
belong elsewhere.

For configured generation, `src/providers/factory.rs` reads Finch provider entries and
credentials, constructs a concrete provider through this crate, and exposes a `ProviderGraph` to
the REPL or daemon. It wraps provider dispatch to enforce credential lifecycle. The crate owns
validated requests and transport behavior; the factory owns application profile selection.

For model setup, `src/cli/setup_wizard/catalog.rs` converts the chosen provider and persisted
entry into a `ModelCatalogProfile`, using this crate's auth and endpoint contract. The setup
wizard asks the catalog for available models and retains the application decision about what to
save. A catalog response is not proof that a model or provider path has passed end-to-end
conformance.

Read [AGENTS.md](AGENTS.md) for dependency and security rules, [src/lib.rs](src/lib.rs) for the
crate facade, and `cargo doc -p finch-providers --no-deps --open` for signatures. The OAuth
state machine has its own [capsule](src/oauth/AGENTS.md). Its currently public `oauth` module
path is a facade exception still to be evaluated before flattening external call sites.
