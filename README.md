# Finch

Finch is an experimental terminal coding assistant written in Rust. It provides an interactive
REPL, provider-backed chat, code and shell tools with an approval boundary, local persistence, an
MCP client, a typed Lisp/Co-Forth runtime, and named shared Brain sessions.

Finch is under active development. This README describes the current `main` branch, not a promise
that every configured provider, local model, or experimental collaboration path is production
ready. See [Current limitations](#current-limitations) before relying on it.

## Quick start from source

The most reliable way to try the current code is to build a clean checkout. Supported CI targets
are Apple Silicon macOS and x86-64 Linux. Install the stable Rust toolchain, Git, and the Cap'n Proto
compiler first.

macOS (Apple Silicon):

```bash
xcode-select --install
brew install capnp
git clone https://github.com/darwin-finch/finch.git
cd finch
cargo build --release --bin finch
./target/release/finch setup
./target/release/finch --cloud-only
```

Ubuntu/Debian Linux (x86-64):

```bash
sudo apt-get update
sudo apt-get install -y build-essential capnproto pkg-config libssl-dev
git clone https://github.com/darwin-finch/finch.git
cd finch
cargo build --release --bin finch
./target/release/finch setup
./target/release/finch --cloud-only
```

`finch setup` writes provider profiles and settings to `~/.finch/config.toml`. The setup UI
currently offers API-key profiles for Anthropic, OpenAI, xAI, Google Gemini, Mistral, and Groq, as
well as local-model configuration. Use a provider model returned by the setup catalog or enter one
explicitly; model availability changes independently of Finch.

GitHub release assets currently exist for Apple Silicon macOS and x86-64 Linux, but they can lag
behind `main`. Release and installer reliability are being tracked in
[#119](https://github.com/darwin-finch/finch/issues/119) and
[#144](https://github.com/darwin-finch/finch/issues/144), so this first truthful refresh does not
recommend the one-line installer as the canonical path.

## Current interfaces

These commands are defined by the current CLI:

```text
finch                         interactive terminal UI
finch --raw                   interactive line-oriented UI
finch --cloud-only            skip local-model loading and the daemon
finch query "explain this"    run one provider-backed query
finch setup                   configure provider profiles and settings
finch daemon                  foreground HTTP server on 127.0.0.1:8000 by default
finch daemon-start            background daemon on 127.0.0.1:11435 by default
finch daemon-status           report background-daemon status
finch daemon-stop             stop the background daemon
finch --forth "1 2 +"         evaluate typed Co-Forth without an LLM
finch --lisp "(+ 1 2)"        evaluate typed Finch Lisp without an LLM
finch --exec path/to/file      execute a typed Finch script
```

Run `finch --help` and `finch <command> --help` for the full generated CLI reference. In the REPL,
`/help` shows the slash commands present in that build. `/model`, `/provider`, and `/teacher`
currently reach the same profile selector; generated help emphasizes `/model` while retaining the
other spellings for compatibility.

## Shared brains (experimental)

Tests and live smokes involving Brains must not be invoked directly. Give the
process a disposable Finch home with the guarded wrapper (credentials needed
by a live smoke should be supplied explicitly through environment variables):

```bash
./scripts/test_brains.sh env FINCH_LIVE_TESTS=1 cargo test --test live -- --ignored
```

The wrapper deletes the temporary store on success or failure and fails if the
real `~/.finch/brains` manifest changes.

A named Brain keeps one ordered conversation and one persistent typed Lisp/Co-Forth VM across
multiple terminals and daemon restarts.

### Tools and approval

Finch can inspect files, search source, propose changes, run commands, and call configured MCP
tools. Approval is policy-dependent: known read-only operations may run without a prompt, while
mutating or otherwise sensitive operations normally require review or are denied. Enabling an
auto-approval setting changes that boundary. The implementation contract is documented in
[`src/tools/EXECUTION.md`](src/tools/EXECUTION.md); treat generated diffs and commands as untrusted
until reviewed.

### Configuration and providers

The authoritative configuration types live in
[`src/config/settings.rs`](src/config/settings.rs) and
[`src/config/provider.rs`](src/config/provider.rs). A minimal API-key profile looks like:

```toml
[[providers]]
type = "claude"
name = "work"
api_key = "sk-ant-..."
```

Provider profiles may have user-defined names and model overrides. The code also contains profile
types for Ollama, a remote Finch daemon, and local inference. Configuration support is not the
same as end-to-end conformance: provider routing and model selection are still being reconciled in
[#51](https://github.com/darwin-finch/finch/issues/51),
[#74](https://github.com/darwin-finch/finch/issues/74),
[#98](https://github.com/darwin-finch/finch/issues/98), and
[#104](https://github.com/darwin-finch/finch/issues/104).

ChatGPT consumer subscriptions are not an authentication mechanism for Finch. Legacy
`chatgpt_subscription` configuration is rejected with migration guidance; subscription/device
authentication remains unresolved and must not be inferred from OpenAI API-key support.

### Local inference

The source contains ONNX Runtime and Candle loaders plus local profiles for several model families.
Local artifacts can be large and may require Hugging Face access. A configured local profile does
not currently guarantee that a query is routed locally; local bootstrap, selection, and provider
parity remain experimental under [#74](https://github.com/darwin-finch/finch/issues/74) and
[#98](https://github.com/darwin-finch/finch/issues/98). Use `--cloud-only` when you need to avoid a
local download attempt.

### HTTP daemon

`finch daemon` binds the foreground HTTP server to `127.0.0.1:8000` unless `--bind` is supplied.
The separately managed background daemon uses `127.0.0.1:11435`. The server implements
`POST /v1/chat/completions`, `GET /v1/models`, `POST /v1/messages`, health/metrics endpoints, and
Finch-specific feedback, node, and Brain routes. It does **not** implement the complete OpenAI API
or the Responses API. Integration conformance is tracked in
[#130](https://github.com/darwin-finch/finch/issues/130),
[#133](https://github.com/darwin-finch/finch/issues/133), and
[#134](https://github.com/darwin-finch/finch/issues/134).

When explicitly enabled, remote Brain collaboration uses a distinct TLS listener whose configured
default is `0.0.0.0:11436`. That listener exposes a restricted Brain route set; it is not the
OpenAI-compatible endpoint.

### MCP client

Finch can start configured stdio MCP servers and import their tools. It does not expose an MCP
server. See the [MCP client guide](docs/MCP_USER_GUIDE.md) for configuration and troubleshooting.

### Brains, typed programs, and subagents

The typed Lisp/Co-Forth runtime and named Brain persistence have extensive implementation and test
coverage, but their user workflows are experimental. A Brain is a named, durable conversation and
program environment owned by the daemon; local and remote attachment have different transport and
authority boundaries. Subagent and remote-collaboration behavior is still evolving under
[#57](https://github.com/darwin-finch/finch/issues/57),
[#107](https://github.com/darwin-finch/finch/issues/107),
[#140](https://github.com/darwin-finch/finch/issues/140),
[#145](https://github.com/darwin-finch/finch/issues/145), and
[#146](https://github.com/darwin-finch/finch/issues/146).

Use direct typed-runtime commands for bounded experiments:

```bash
./target/release/finch --forth "1 2 +" --json
./target/release/finch --lisp "(+ 1 2)" --json
```

## Persistence and privacy

Finch stores configuration and application state under `~/.finch/`, including SQLite memory,
sessions, named Brains, feedback, and approval patterns as applicable. Explicit good/bad response
feedback is private durable metadata; it does not authorize or trigger training. Legacy training
queues and adapters may be preserved but are not processed automatically.

Local storage does not make a cloud-backed session offline: prompts, selected context, and tool
results sent to a configured cloud provider leave the machine and are governed by that provider.
Configured MCP servers and remote Finch peers can also receive data as directed by their tools and
capabilities. Finch has no automatic LoRA training path; see
[#139](https://github.com/darwin-finch/finch/issues/139).

## Current limitations

- Finch is experimental and is not presented as release-ready or production-ready.
- Provider and local-model behavior is not yet uniform; verify the exact profile and model you use.
- There is no supported ChatGPT subscription/device-auth flow and no live-model claim for any
  unreleased or unverified model.
- The server supports a small endpoint subset, not the full OpenAI API and not `/v1/responses`.
- Image-input/output support is not documented as available.
- Finch does not automatically update itself, train LoRA adapters, or turn feedback into training.
- Brain, remote collaboration, typed-agent, and subagent workflows remain experimental.

The issue tracker is the source for planned work. In particular, do not treat design documents as
implemented behavior unless current source or tests say so.

## Development

After installing the prerequisites from [Quick start from source](#quick-start-from-source):

Source builds use the repository-pinned Rust 1.98.0 toolchain. See
[`docs/RUST_TOOLCHAIN.md`](docs/RUST_TOOLCHAIN.md) for the tested-toolchain and MSRV policy.

```bash
cargo fmt --all -- --check
cargo check --lib --bins --tests
cargo test
python3 scripts/check_docs.py
```

Some full builds and tests are memory-intensive. Narrow commands to the module under change when
appropriate, then rely on CI for the supported platform matrix. See [CONTRIBUTING.md](CONTRIBUTING.md)
before submitting work and [docs/README.md](docs/README.md) for the documentation map.

## Maintainer and development assistance

Finch was created and is maintained by **Shammah Chancellor**. Substantial portions of the project
have been developed with assistance from Anthropic Claude and OpenAI Codex. Those products are
development tools, not legal authors, maintainers, people, or GitHub identities. Commit authorship
should identify the human responsible for the change; optional assistance trailers must be
truthful. Existing contributor displays are a consequence of historical commit metadata and cannot
be repaired without rewriting history, which this project does not do for attribution cleanup.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the attribution policy.

## License

Finch is source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE). Review the license text for the authoritative
terms. Commercial licensing information is available at <https://polar.sh/darwin-finch>.
