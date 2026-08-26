# CLAUDE.md - AI Assistant Context

This document orients AI assistants working on the Finch project. Implementation detail lives in co-located module docs; this file covers the why, shared guidelines, and behavioral invariants.

## Project Context

**Project Name**: Shammah (שָׁמָה - "watchman/guardian")
**Binary**: `finch`
**Purpose**: Local-first AI coding assistant with explicit private feedback
**Core Innovation**: Local ONNX inference across 6 model families; Apple Silicon acceleration via CoreML EP; cloud fallback during bootstrap

**The Problem:** Traditional AI assistants require constant internet, incur API costs per query, and can't learn your patterns.

**The Solution:** Finch runs locally after a one-time model download (<100ms startup, near-zero marginal cost per query, offline-capable), with cloud fallback while the local model loads on first run.

**Key Metrics:**
- Startup: <100ms (instant REPL)
- First-run: 0ms blocked (background download)
- System support: 8GB–64GB+ RAM (adaptive model selection)

## Architecture Overview

```
User Query
    ↓
Router (model ready? → local; loading? → teacher API)
    ↓
ONNX Local Model (Qwen/Llama/Gemma/Mistral/Phi/DeepSeek)
CoreML EP on macOS · CUDA/ROCm/CPU on Linux
    ↓
Response
```

### Module Docs

| Component | Module Doc |
|-----------|-----------|
| Progressive Bootstrap | `src/models/BOOTSTRAP.md` |
| ONNX Model Integration | `src/models/ONNX.md` |
| LoRA Fine-Tuning | `src/models/LORA.md` |
| Router | `src/router/ROUTING.md` |
| TUI Renderer | `src/cli/tui/ARCHITECTURE.md` · `docs/TUI_ARCHITECTURE.md` |
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

### Exchange (peer function exchange via shared channel)

- **`finch exchange run` never executes without user confirmation** — the daemon returns what would run; the CLI must print it and require explicit `--yes` or interactive confirmation before executing.
- **A peer's proposal does not install into the local VM until the local user accepts** — `finch exchange run` installs nothing silently; rejection leaves the VM unchanged.
- **Forked sessions share base vocabulary implicitly** — two sessions running the same binary start from identical stdlib + builtins; the exchange channel carries only the delta.
- **Proposals are visible before execution** — `finch exchange list` always shows the full program text, not just a name, so the user can read what they are accepting.
- **Rejection is a first-class operation** — clearing or ignoring a proposal must be as easy as accepting it; no proposal should require action to dismiss.

## Key Design Decisions

### Pre-trained models (not training from scratch)

Using pre-trained Qwen models gives immediate quality from day 1 with no cold-start period. LoRA training and adapter loading remain blocked on Issues #1, #7, and #74.

### Weighted feedback

Three historical weight tiers are retained for explicit feedback: high (10x), medium (3x), normal (1x). `Ctrl+G` = good, `Ctrl+B` = bad. Feedback is private durable data; it does not trigger training.

### Progressive Bootstrap

REPL appears in <100ms; model loads in the background. Queries forward to teacher API (Claude/GPT-4/etc.) until `GeneratorState::Ready`. See `src/models/BOOTSTRAP.md`.

### ONNX over Candle on macOS

`candle-metal` is missing layer-norm kernels and matmul dimension combinations for Qwen. `candle-coreml` requires incompatible ANEMLL format. ONNX + CoreML EP is the only reliable macOS path. See `src/models/ONNX.md`.

### Storage layout

```
~/.finch/
├── config.toml          # User config
├── adapters/            # Preserved legacy adapters; not loaded automatically
├── feedback.jsonl       # Private explicit feedback; never a training trigger
├── training_queue.jsonl # Preserved legacy queue; not processed automatically
├── metrics/             # Usage metrics
└── tool_patterns.json   # Approved tool patterns

~/.cache/huggingface/hub/  # Base models (HF standard)
```

### Operating modes

- **Interactive REPL:** `finch`
- **Single query / pipe:** `finch query "..."` or `echo "..." | finch`
- **Daemon (auto-spawned, OpenAI-compatible API):** `finch daemon --bind 127.0.0.1:11435`
  - VS Code / Continue.dev: point at `http://localhost:11435`, provider = `openai`, model = `local`
  - mDNS discovery: `finch daemon --bind 0.0.0.0:11435 --mdns`

## Technology Stack

- **Language:** Rust (memory safety, performance, Apple Silicon support)
- **ML Framework:** ONNX Runtime (`ort` crate) — primary; Candle (Linux/CPU alt)
- **Async:** Tokio
- **HTTP server:** Axum (daemon, OpenAI-compatible API)
- **TUI:** Ratatui + crossterm
- **Key deps:** `hf-hub`, `tokenizers`, `indicatif`, `sysinfo`

**Supported model families (ONNX):** Qwen 2.5, Llama 3, Gemma 2, Mistral, Phi, DeepSeek Coder
**Teacher providers:** Claude, GPT-4, Gemini, Grok, Mistral, Groq

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
cargo test                          # all tests
cargo test --lib tools::permissions # specific module
cargo test -- --nocapture           # with output
```

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

GitHub Actions builds `finch-macos-arm64.tar.gz` (macOS 14 runner) and `finch-linux-x86_64.tar.gz` (ubuntu-24.04+).

**Platform notes:**
- Intel macOS: **not supported** (`ort` has no prebuilt binaries; GitHub deprecated Intel Mac runners Jun 2025)
- Linux: must be `ubuntu-24.04`+ (requires glibc 2.38+)
- macOS-only deps in `Cargo.toml` must appear **before** the `[target.'cfg(target_os = "macos")'.dependencies]` header

## Current Project Status

**Version**: 0.7.6 (Feb 2026)

| Capability | Status |
|-----------|--------|
| Local ONNX inference | ✅ |
| 6 model families | ✅ |
| 6 teacher providers | ✅ |
| TUI with scrollback/streaming | ✅ |
| Daemon (OpenAI-compatible API) | ✅ |
| mDNS discovery | ✅ |
| Private explicit feedback collection | ✅ |
| LoRA training + adapter loading | Blocked: Issues #1/#7/#74 |
| Mistral ONNX | ⏳ Issue #2 |

### Open Issues

See **https://github.com/darwin-finch/finch/issues**

## Reference Documents

| Document | Purpose |
|----------|---------|
| `README.md` | User-facing documentation |
| `CHANGELOG.md` | Version history |
| `docs/ROADMAP.md` | Future work planning |
| `docs/ARCHITECTURE.md` | System architecture overview |
| `docs/DAEMON_MODE.md` | Daemon architecture details |
| `docs/TUI_ARCHITECTURE.md` | TUI rendering (full detail) |
| `docs/TOOL_CONFIRMATION.md` | Tool permission system |
| `docs/MODEL_BACKEND_STATUS.md` | Model backend comparison |
| `docs/USER_GUIDE.md` | Setup and usage |

## Key Principles

1. **Immediate Quality** — pre-trained models work day 1
2. **Explicit Feedback** — user ratings are retained privately
3. **User Control** — feedback never implies consent to train
4. **Privacy First** — local inference, offline capability
5. **Professional UX** — instant startup, graceful degradation
6. **Rust Best Practices** — safe, idiomatic, performant code

---

*If you're unsure: check this file → check module docs → check README.md → look at existing code → ask.*
