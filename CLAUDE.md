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

## Architecture and subsystem capsules

[`DESIGN.md`](DESIGN.md) describes how Finch is composed: subsystem ownership, dependencies,
module documentation, operating modes, storage layout, and the technology stack.
[`subsystems.toml`](subsystems.toml) is the authoritative ownership and dependency record.

### Subsystem capsules

Before editing under a path listed here, read its capsule. A capsule adds subsystem-specific
scope, dependency limits, and focused tests to everything in this file; it never replaces it.

| Paths | Capsule |
|-------|---------|
| `src/vm/`, `src/lisp/`, `vocabulary/`, `examples/finch/` | [`src/vm/AGENTS.md`](src/vm/AGENTS.md) |
| `src/memory/`, `src/memory_status.rs`, `src/workbook.rs` | [`src/memory/AGENTS.md`](src/memory/AGENTS.md) |
| `src/programs/` | [`src/programs/AGENTS.md`](src/programs/AGENTS.md) |
| `src/config/`, `src/context/`, `src/license/`, `src/metrics/` | [`src/config/AGENTS.md`](src/config/AGENTS.md) |

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

- **Load order: `AGENTS.md` → `CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md`; cwd wins over parent; a file reached by several names (symlink, hardlink) loads once** — `loads_all_names_in_same_directory`, `joins_multiple_sections_with_separator`, `symlinked_agents_md_loads_once_at_the_later_position` in `src/context/claude_md.rs`; `provider_request_carries_symlinked_agents_md_once_and_nested_rules_last` in `src/generators/claude.rs`

### Subsystem interfaces

- **Every subsystem's `INTERFACE.md` matches its facade** — an agent must be able to read what a
  subsystem offers without opening its source, so the file is generated and checked, never hand-
  edited. Change a public item, then run `python3 scripts/generate_interfaces.py --write` and commit
  the result — `scripts/test_generate_interfaces.py`, and the check in `repository-hygiene.yml`

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
remain experimental under Issues #74 and #98. LoRA training and adapter loading are deferred because
their runtime and ML-toolchain requirements are unsupported; the automatic Python path was disabled
until a supported path exists. Preserved legacy queues and adapters are not processed automatically.
Treat the model catalog, setup choices, and loaders as configuration surfaces—not claims that each
combination has passed conformance.

Feedback weights, the local backend investigation, storage layout, and operating modes are
described in [`DESIGN.md`](DESIGN.md#runtime-reference).

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

### Reporting status to a human

**Never cite a bare issue or pull request number.** An identifier is meaningless to a
reader who cannot look it up while reading. Every mention carries a short
plain-language description of what the thing actually is.

```text
# ✅ Legible
#381 (log spam — the daemon repeated one warning every minute)
Fast launch (#372, merged): /health no longer hydrates every Brain

# ❌ Unreadable
#381 is in review, #364 is blocked on #377, and #371 is unclaimed.
```

Applies to status updates, tables, handoffs, commit messages, and passing
references — to other agents' work as much as your own. In a table, give the
description its own column rather than appending it to the number.

Two related habits, for the same reason:

- **Say what changed for the user, not only what the patch did.** "Startup went from
  about three seconds to instant" tells a maintainer something; "replaced `list()`
  with `count_unhydrated()`" tells them only where to look.
- **Name the file or symbol, not just the layer.** `src/brain/store.rs:1528`
  (`BrainStore::list`) is checkable; "the store" is not.

## Release Process

Follow the steps in [`CONTRIBUTING.md`](CONTRIBUTING.md#release-process). Do not describe a release
as ready merely because artifacts exist; release reliability is tracked in #119 (signed packages and
verified rollback) and #144 (newer-release notification). macOS-only dependencies belong **after**
the `[target.'cfg(target_os = "macos")'.dependencies]` header so they remain target-scoped. The Linux
release runner must stay on `ubuntu-24.04` or newer (glibc 2.38+), and Intel macOS is unsupported.

## Current Project Status

`Cargo.toml` is authoritative for the source version. Finch is experimental. The interactive CLI,
typed runtime, bounded HTTP routes, local persistence, MCP client, and explicit feedback store have
implementation and tests. Provider/local routing parity, remote collaboration, subagents, and
release integration remain active work. Automatic training and LoRA adapter loading are deferred.

### Open Issues

See **https://github.com/darwin-finch/finch/issues**

## Reference Documents

| Document | Purpose |
|----------|---------|
| `README.md` | User-facing documentation |
| `DESIGN.md` | Root architecture and subsystem index |
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
