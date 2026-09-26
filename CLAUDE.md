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

[`DESIGN.md`](DESIGN.md) describes how Finch is composed: what the modules are, how they depend on
each other, operating modes, storage layout, and the technology stack. The tree itself is the
record — a directory's `AGENTS.md` states its working contract, its `README.md` explains its
purpose when present, and its Rust facade states what it exports.

### Read the local agent contract and narrative before the facade

A source directory's `AGENTS.md` states what that subtree owns, what it may depend on, its
invariants, and how to test it. A `README.md`, where present, explains why it exists and shows
caller workflows. **Read both before inspecting that subtree**, then read `mod.rs` or a crate's
`src/lib.rs` for the exported surface. These documents supplement this file; neither replaces it.

To call into another module, use its README and agent contract for meaning and constraints, its
facade for exports, and rustdoc for methods on re-exported types. Rustdoc derives the callable
signatures from source; do not recreate them as a checked-in symbol catalog. If that route still
requires reading private implementations to answer a caller task, record the gap and improve the
boundary instead of inventing a generic abstraction.

**Keeping it true is part of the change, not follow-up work.** If you alter what a module owns or
depends on, update its `AGENTS.md` in the same commit. If you change its purpose or caller
workflow, update its `README.md`. A capsule that describes an obsolete boundary is worse than
none. Do not recreate generated `INTERFACE.md` files; the human README and AGENTS contract
explain meaning, and the Rust facade plus rustdoc show the callable surface.

A module directory earns a capsule when something outside it depends on it. For a root-package
module, `mod.rs` is the facade; for a library crate, `src/lib.rs` is the crate-root facade. Keep
child modules private and use deliberate flat re-exports. The facade identifies entry points;
rustdoc shows methods on re-exported types without duplicating their signatures in prose.

## Invariants

Behaviors that **must always be true**. If a test doesn't exist for a claim below, treat it as a bug.

### Security

- **Peer cannot restart, spawn processes, or delegate to Claude Code** — the deny keys on the tool names `RestartTool`, `TaskTool`, and `ClaudeCodeDelegateTool` actually register (`PEER_HARD_DENY_TOOLS`), not on literals; `test_peer_cannot_restart`, `test_peer_cannot_spawn`, `test_peer_cannot_delegate_to_claude_code` in `src/tools/permissions/tests.rs` exercise the real deny path through those names at the composition root, and `test_peer_hard_deny_table_names_are_declared_by_real_tool_implementations` fails if the table drifts onto a name nothing registers or a declaration stops matching
- **Tools declare their execution authority** — every `Tool` implements `effect()` with no default (deleting one fails to compile), no string-keyed effect table remains, and `test_declared_effects_match_pre_refactor_classification` in `src/cli/repl/always_allow_tests.rs` pins every registered tool's declaration to its pre-refactor classification — `test_declared_effect_is_independent_of_dispatch_spelling` in the same file fails if authority depends on the spelling a provider emits
- **`is_readonly_bash()` rejects commands with shell operators** — any `;`, `|`, `>`, `<`, `&` in the command returns `false`, preventing prefix-bypass attacks — `test_is_readonly_bash_pipe_chain_is_rejected`, `test_is_readonly_bash_redirect_is_rejected` in `crates/finch-tools-api/src/permissions.rs` (moved with the permission policy into the dependency-free tools API crate, issue #872); the readonly refinement stays at the approval call sites (`refined_effect_for_approval`), and `test_bash_readonly_refinement_still_rejects_shell_operators` proves it survives the declared-effect re-key
- **Peer read/glob/grep are silently allowed; write/edit/patch surface as AskUser** — `test_peer_read_glob_grep_silently_allowed`, `test_peer_write_edit_patch_surfaces_as_ask` in `crates/finch-tools-api/src/permissions.rs` (moved with the policy, issue #872)
- **Constitutional constraints apply to peers too** — `test_peer_constitutional_constraints_still_apply` in `crates/finch-tools-api/src/permissions.rs` (moved with the policy, issue #872)
- **License: malformed base64 returns `Err`, never panics** — `test_validate_key_*` in `src/license/mod.rs`

### Routing

- **Router forwards to a cloud provider for ALL queries while model is loading** — `test_route_with_generator_not_ready_always_forwards` in `src/router/decision.rs`
- **Router does not return `ModelNotReady` when generator IS ready** — `test_route_with_generator_ready_uses_normal_routing` in `src/router/decision.rs`

### TUI

- **Scrollback deduplication: each message is written to the terminal exactly once** — `commit_complete_messages` skips ids already in `printed_ids` and marks a message only after its staged bytes have been written, so a flush that reports an ambiguous error is never retried into a duplicate; after a resize clears the screen, `prepare_canonical_commit` removes the visible projection first so a re-commit cannot spool the row into native history twice — `canonical_commit_marks_only_after_success_and_follows_resize_clear` in `crates/finch-tui/src/lib.rs`
- **Peer/home/runner IPC diagnostics are status/header, never transcript rows** — disconnect and reconnect attempts update `StatusBar` `SessionLabel` and must not become `OutputManager` conversation rows between program source and output — `peer_ipc_diagnostics_after_completed_turn_stay_off_transcript` in `src/cli/repl_event/event_loop/tests.rs`
- **Dialog virtual rows stable after Other-row activation** — `test_multiselect_submit_button_emits_selection`, `test_o_key_moves_cursor_to_other_row` in `crates/finch-tui/src/dialog.rs`
- **Prose must never be executed as a typed program** — `is_clearly_forth` in `src/main.rs` decides this for `finch query`; a question mark, comma, apostrophe or leading capital disqualifies a line, and the apostrophe test runs before the operator-character test so an emphatic contraction is not claimed by `!` — `prose_about_a_forth_string_opener_is_not_executed_as_forth`, `test_contraction_with_emphasis_is_not_executed_as_forth`, `test_a_forth_line_with_an_operator_still_runs` in `src/main.rs`
- **Component renderers construct no SGR; styling flows from the injected palette** — `test_component_renderers_construct_no_sgr_bytes` in `crates/finch-ui-model/src/component.rs` and the wizard mirror `test_wizard_view_builders_construct_no_sgr_bytes` in `src/cli/setup_wizard/tests.rs` (stage 4, #1141); spans lower at the two paint seams only and the canonical commit keeps its raw bytes — `test_component_spans_lower_to_sgr_at_the_live_frame_paint` in `crates/finch-tui/src/lib.rs`
- **Wizard selections, the active tab, and the context-lines spinner are visibly styled, not prefix-only** — the selection style is bold bright-white on black and the active tab bold magenta on black, driven by props — `test_selected_rows_carry_selection_contrast_beyond_the_prefix`, `test_active_tab_marker_prop_selects_the_highlighted_tab`, `test_context_lines_spinner_value_is_visible_keys_adjust_and_keys_are_advertised` in `src/cli/setup_wizard/tests.rs` (#1140)

### Request assembly

- **Summarised request prefix is byte-stable across turns, with the stable system/context prefix preceding the summary** — past `max_verbatim`, the summary is committed to a range that only moves when the window slides past it (`SummaryCache` in `src/cli/conversation_compactor.rs`), so the head of the message array is reused byte-for-byte instead of regenerated per turn — `test_summarised_request_prefix_is_byte_stable_across_turns` in `src/cli/repl_event/query_processor.rs`

### Context

- **Load order: `AGENTS.md` → `CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md`; cwd wins over parent; a file reached by several names (symlink, hardlink) loads once** — `loads_all_names_in_same_directory`, `joins_multiple_sections_with_separator`, `symlinked_agents_md_loads_once_at_the_later_position` in `src/context/claude_md.rs`; `provider_request_carries_symlinked_agents_md_once_and_nested_rules_last` in `src/generators/claude.rs`

### Subsystem interfaces

- **The facade defines the public surface** — child modules stay private and callers enter through
  `mod.rs` or `src/lib.rs`. Modules with external callers carry a human README and AGENTS
  contract; generated `INTERFACE.md` catalogs are retired and must not return.

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
- **Do not use precision or comparative timing as the sole correctness oracle** when
  deterministic state, event, or order assertions are available. Assert the structural fact
  instead: hydration state, resident counts, phase order, event sequence, exactly-once
  terminal state. Commit `a0ea2c64` ("assert hydration state, not a wall-clock ratio") is
  the precedent for the prohibited shape — a ratio of two measured durations that passed
  on a fast machine with the defect present and failed on a loaded one without it. That
  commit repaired one flaky test; it did not establish this rule. Coarse liveness bounds
  whose failure message says the run hung rather than that it was slow remain permitted
  (supervisor, service-discovery, and provider-isolation timeouts already work this way).
  Explicitly authorised benchmark or budget guards, paired with semantic assertions, also
  remain permitted: #282 (two-cell spreadsheet exhausts memory) keeps a coarse elapsed
  bound next to semantic error assertions, and #242 (prompt-first TUI startup) requires a
  documented warm-start latency and RSS budget with a CI guard.
- **A green unrelated suite is not regression evidence** — name the test that reproduces the bug
  in the commit message and GitHub verification comment, and record why it failed before the fix.
- **Manual verification does not replace regression coverage** — document any manual evidence, but
  keep the issue and branch unmerged until the failure has a deterministic automated regression.
- **Every agreed-upon behavior must be covered** — if it's worth discussing, it's worth testing.
- **Unit tests live in the same module** as the code they test — inline
  (`#[cfg(test)] mod tests { ... }`) or, when the file is large enough that its tests bury it, a
  sibling file the module declares (`#[cfg(test)] mod tests;` beside `thing.rs` in `thing/tests.rs`).
  Rust treats both as the same module, so `use super::*` still reaches private items either way.
  Do not mix the two in one file. Production-boundary and executable-level regressions may live
  under `tests/` with shared fixtures.
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

### Use context economically

Report outcomes, decisions, blockers, failures, review findings, and integrations; omit routine
narration and successful intermediate steps. Search and read narrowly, bound tool output, retain
actionable failure context, and start with the smallest relevant gate. Record durable evidence in
an existing issue or pull request instead of repeating it in conversation. Final handoffs contain
only the change, verification, remaining risk, ownership, and next action.

For multi-step or delegated work, follow
[efficient execution and evidence](.agents/skills/finch-backlog/references/execution-efficiency.md).
Parallelism and token efficiency never reduce required proof, testing, review, safety, or user
value, and hard token budgets must not truncate work or evidence.

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
