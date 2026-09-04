# CLAUDE.md - AI Assistant Context

This document orients AI assistants working on the Finch project. Implementation detail lives in co-located module docs; this file covers the why, shared guidelines, and behavioral invariants.

## Project Context

**Project Name**: Finch
**Binary**: `finch`
**Purpose**: Experimental terminal coding assistant with provider-backed chat, typed programs,
named Brains, tool use, and explicit private feedback

Finch is under active development. Configuration variants and loader code are not proof of
end-to-end provider or local-model conformance. Do not repeat performance, offline, model-support,
or release-readiness claims without dated evidence; see Issues #74, #98, #120, and #147.

## Architecture Overview

```
CLI / query client
    ↓
configured provider graph or daemon client
    ↓
provider transport and/or experimental local generator
    ↓
typed runtime + capability broker for program effects
```

### Module Docs

| Component | Module Doc |
|-----------|-----------|
| Local model loader | `src/models/unified_loader.rs` · `src/models/ONNX.md` |
| Disabled LoRA path | `docs/AUTOMATIC_TRAINING.md` · `src/models/LORA.md` |
| Router | `src/router/ROUTING.md` |
| TUI Renderer | `src/cli/tui/ARCHITECTURE.md` |
| Tool Execution & Permissions | `src/tools/EXECUTION.md` |
| Claude Client | `src/claude/CLIENT.md` |
| Context Assembly | `src/context/ASSEMBLY.md` |
| Configuration | `src/config/CONFIGURATION.md` |
| License System | `src/license/LICENSING.md` |

## Invariants

Behaviors that **must always be true**. If a test doesn't exist for a claim below, treat it as a bug.

### Security

- **Peer cannot restart or spawn processes** — `test_peer_cannot_restart`, `test_peer_cannot_spawn` in `src/tools/permissions.rs`
- **`is_readonly_bash()` rejects commands with shell operators** — any `;`, `|`, `>`, `<`, `&` in the command returns `false`, preventing prefix-bypass attacks — `test_is_readonly_bash_pipe_chain_is_rejected`, `test_is_readonly_bash_redirect_is_rejected` in `src/tools/permissions.rs`
- **Peer read/glob/grep are silently allowed; write/edit/patch surface as AskUser** — `test_peer_read_glob_grep_silently_allowed`, `test_peer_write_edit_patch_surfaces_as_ask` in `src/tools/permissions.rs`
- **Constitutional constraints apply to peers too** — `test_peer_constitutional_constraints_still_apply` in `src/tools/permissions.rs`
- **License: malformed base64 returns `Err`, never panics** — `test_validate_key_*` in `src/license/mod.rs`

### Routing

- **Router forwards to teacher for ALL queries while model is loading** — `test_route_with_generator_not_ready_always_forwards` in `src/router/decision.rs`
- **Router does not return `ModelNotReady` when generator IS ready** — `test_route_with_generator_ready_uses_normal_routing` in `src/router/decision.rs`

### TUI

- **Scrollback deduplication: each message written via `insert_before()` exactly once** — check `scrollback.get_message(msg_id).is_none()` before calling (tests in `src/cli/tui/scrollback.rs`)
- **Dialog virtual rows stable after Other-row activation** — `test_multiselect_submit_button_emits_selection`, `test_o_key_moves_cursor_to_other_row` in `src/cli/tui/dialog.rs`
- **Contractions and sentence-ending periods route to NL, not Forth** — `test_contraction_dont`, `test_period_attached_to_word` in `src/cli/repl_event/event_loop.rs`

### Context

- **Load order: `CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md`; cwd wins over parent** — `loads_all_names_in_same_directory`, `joins_multiple_sections_with_separator` in `src/context/claude_md.rs`

### GUI Accessibility

- **Coordinate-based GUI ops are forbidden as the primary interface** — blind users cannot determine pixel positions; all GUI tools must accept semantic identifiers: element role + label, button name, or app-domain address (e.g. cell `B3` in Excel).
- **Every GUI read must return plain text** — no visual-only confirmation; results must be fully speakable and meaningful without seeing the screen.
- **App-specific words must exist for common applications** — `excel-read`, `excel-write`, `excel-cell` etc. so a blind user can say `"B3" excel-read` and get the cell value as text, without knowing anything about coordinates or window layout.
- **GUI errors must name what was not found** — "button 'Save' not found in Excel" not "click failed"; the error text must be actionable by someone who cannot see the screen.
- **`gui_click` with raw coordinates is an internal primitive, not a user-facing tool** — it must not appear in the default tool list for non-developer personas.
- **Accessibility permission errors must explain how to fix them** — the error message must include the exact path to grant access (`System Settings → Privacy & Security → Accessibility`).

## Key Design Decisions

### Provider and local-model claims

Use current provider profiles and pre-trained local artifacts only. Local routing and provider parity
remain experimental under Issues #74 and #98. LoRA training and adapter loading are disabled under
Issue #139; preserved legacy queues and adapters are not processed automatically.

### Weighted feedback

Three historical weight tiers are retained for explicit feedback: high (10x), medium (3x), normal (1x). `Ctrl+G` = good, `Ctrl+B` = bad. Feedback is private durable data; it does not trigger training.

### Local backend investigation

The source contains ONNX Runtime and Candle loaders. Historical backend experiments are recorded in
`docs/MODEL_BACKEND_STATUS.md`, but that document is not end-to-end routing or conformance evidence.

### Storage layout

```
~/.finch/
├── config.toml          # User config
├── adapters/            # Preserved legacy adapters; not loaded automatically
├── feedback.jsonl       # Private explicit feedback; never a training trigger
├── training_queue.jsonl # Preserved legacy queue; not processed automatically
├── metrics/             # Usage metrics
├── notice_state.toml    # Licence-notice bookkeeping; kept out of config.toml (#76)
├── tool_patterns.json   # Approved tool patterns
├── sessions/            # Saved REPL sessions
└── brains/              # Named Brain event logs and state

~/.cache/huggingface/hub/  # Base models (HF standard)
```

### Operating modes

- **Interactive REPL:** `finch`
- **Single query / pipe:** `finch query "..."` or `echo "..." | finch`
- **Foreground HTTP server:** `finch daemon` (default `127.0.0.1:8000`)
- **Managed background daemon:** `finch daemon-start` (default `127.0.0.1:11435`)
- **Restricted remote Brain TLS listener:** configured default `0.0.0.0:11436`; opened only when
  service advertisement is enabled
- **Direct typed programs:** `finch --forth`, `finch --lisp`, and `finch --exec`

Brain and daemon tests must use the isolated launchers and kernel-assigned endpoints.

## Technology Stack

- **Language:** Rust (memory safety, performance, Apple Silicon support)
- **ML frameworks in source:** ONNX Runtime (`ort` crate) and Candle
- **Async:** Tokio
- **HTTP server:** Axum (`/v1/chat/completions`, `/v1/models`, `/v1/messages`, and Finch-specific
  routes; not the full OpenAI API and not the Responses API)
- **TUI:** Ratatui + crossterm
- **Key deps:** `hf-hub`, `tokenizers`, `indicatif`, `sysinfo`

Provider profile variants and local model repositories are defined in source. Treat the model
catalog, setup choices, and loaders as configuration surfaces—not claims that each combination has
passed conformance.

## Development Guidelines

### Code style

- `cargo fmt` before every commit; address `cargo clippy` warnings
- Doc comments on all public items
- **Early exit pattern** — return early for error cases; avoid nesting

```rust
// ✅ Preferred
fn process(config: &Config) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }
    do_work(config)?;
    Ok(())
}
```

### Error handling

```rust
use anyhow::{Context, Result};

fn load_config() -> Result<Config> {
    let path = config_path().context("Failed to determine config path")?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    toml::from_str(&contents).context("Failed to parse config TOML")
}
```

### Testing (mandatory)

- **Every bug fix must have a regression test** that fails before and passes after. No exceptions.
- **Assertion failures must be actionable** — nontrivial assertions must name the behavioral
  invariant and include the diagnostic or state payload needed to explain a failure. Do not rely on
  the test name for context because parallel test output can interleave. For execution outcomes,
  include relevant diagnostics, VM diagnostics, captured output, and identity/timing data as
  applicable; a bare `left == right` status comparison is insufficient.
- **Reproduce the reported failure at the production boundary** — a helper-only unit test is
  insufficient when the bug crossed the TUI, provider, persistence, authority, runner, IPC, or
  process-lifecycle boundary. Add a deterministic production-boundary test that exercises the real
  path; cross-crate or executable-level cases belong in the integration-test tree.
- **Test the hostile timing and restart cases** for concurrent or durable behavior: cancellation,
  disconnect, timeout, late completion, replacement connection, retry, restart, and replay as
  applicable. Assert exact-once terminal state and absence of post-terminal effects.
- **A green unrelated suite is not regression evidence** — name the test that reproduces the bug
  in the commit message and GitHub verification comment, and record why it failed before the fix.
- **Manual verification does not replace regression coverage** — document any manual evidence, but
  keep the issue and branch unmerged until the failure has a deterministic automated regression.
- **Every agreed-upon behavior must be covered** — if it's worth discussing, it's worth testing.
- **Unit tests live in the same file** as the code they test (`#[cfg(test)] mod tests { ... }`);
  production-boundary and executable-level regressions may live under `tests/` with shared fixtures.
- **Naming:** `test_<thing>_<behavior>` e.g. `test_peer_cannot_restart`
- **Mocks for trait contracts** — use `#[ignore]` for tests requiring real model downloads
- **Stubs must have tests** confirming they return errors (not panic)

```bash
./scripts/test_brains.sh cargo test                          # all tests
./scripts/test_brains.sh cargo test --lib tools::permissions # specific module
./scripts/test_brains.sh cargo test -- --nocapture           # with output
```

Never launch Brain, daemon, server, TUI, or live tests directly. Use
`scripts/test_brains.sh` or a launcher that re-executes through it. Test
daemons must bind `127.0.0.1:0`, use a disposable HOME/socket, and remain in the
Rust test supervisor's owned process group. Launchers never signal PIDs; the
supervisor terminates, proves quiescence, and reaps the group before HOME
cleanup. Trusted test code must not enable job control or call `setsid`,
`setpgid`, or `CommandExt::process_group`; the isolation self-test scans the
supervised launchers and daemon paths for these escape APIs. Isolated tests
reject daemon discovery, reuse, and auto-spawn.

### Logging

```rust
use tracing::{debug, info, warn, error};

#[instrument]
async fn load_model(config: &Config) -> Result<Model> {
    info!("Loading model");
    debug!(?config, "Configuration");
    let model = Loader::load(config).context("Failed to load")?;
    info!("Model loaded");
    Ok(model)
}
```

## Release Process

```bash
# 1. Bump version in Cargo.toml
# 2. Commit
git add Cargo.toml && git commit -m "chore: bump version to vX.Y.Z"
# 3. Tag — triggers GitHub Actions release workflow
git tag vX.Y.Z && git push origin main && git push origin vX.Y.Z
```

GitHub Actions is configured to build `finch-macos-arm64.tar.gz` (macOS 14 runner) and
`finch-linux-x86_64.tar.gz` (Ubuntu 24.04 runner). Do not describe a release as ready merely because
artifacts exist; release and installer reliability are tracked in Issues #119 and #144.

**Platform notes:**
- Intel macOS: **not supported** (`ort` has no prebuilt binaries; GitHub deprecated Intel Mac runners Jun 2025)
- Linux: must be `ubuntu-24.04`+ (requires glibc 2.38+)
- macOS-only dependencies belong **after** the `[target.'cfg(target_os = "macos")'.dependencies]`
  header so they remain target-scoped

## Current Project Status

`Cargo.toml` is authoritative for the source version. Finch is experimental. The interactive CLI,
typed runtime, bounded HTTP routes, local persistence, MCP client, and explicit feedback store have
implementation and tests. Provider/local routing parity, remote collaboration, subagents, and
release integration remain active work. Automatic LoRA training is disabled.

### Open Issues

See **https://github.com/darwin-finch/finch/issues**

## Reference Documents

| Document | Purpose |
|----------|---------|
| `README.md` | User-facing documentation |
| `CONTRIBUTING.md` | Contributor setup and attribution policy |
| `docs/README.md` | Current/reference/design/archive documentation map |
| `CHANGELOG.md` | Version history; not current capability evidence |

## Key Principles

1. **Evidence before claims** — configuration or design intent is not conformance
2. **Explicit feedback** — user ratings are retained privately
3. **User control** — feedback never implies consent to train
4. **Capability boundaries** — host effects require typed authority and policy review
5. **Accessible interfaces** — semantic, text-returning automation is the public contract
6. **Rust best practices** — safe, idiomatic, tested code

---

*If you're unsure: check this file → check module docs → check README.md → look at existing code → ask.*
