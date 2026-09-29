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

- **Peer cannot restart or spawn processes** — the deny keys on the tool names `RestartTool` and `TaskTool` actually register (`PEER_HARD_DENY_TOOLS`), not on literals; `test_peer_cannot_restart`, `test_peer_cannot_spawn` in `src/tools/permissions/tests.rs` exercise the real deny path through those names at the composition root, and `test_peer_hard_deny_table_names_are_declared_by_real_tool_implementations` fails if the table drifts onto a name nothing registers or a declaration stops matching
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
- **A toggle-driven wizard description that isn't boxed must declare a fixed row height, not rely on the terminal's own line wrap** — the row-diff blit (`WizardHost::paint` in `crates/finch-tui/src/wizard_host.rs`) clears only the row it repaints and skips a logical line whose content is unchanged without checking whether that line's physical row moved, so a raw un-wrapped `WizardLine` whose toggle state shrinks it from N rows to fewer strands the longer variant's trailing wrapped row on screen; `wizard_boxed` bodies are immune because every physical row is already its own logical line. `local_helpers_section_lines` in `src/cli/setup_wizard/render.rs` wraps both toggle variants with `wizard_wrap` and pads to their combined max height (`wrapped_and_padded`) — `test_local_helpers_toggle_off_does_not_strand_the_on_descriptions_second_row` in `src/cli/setup_wizard/tests.rs` (#1297) replays the real two-frame `WizardHost::paint` byte stream through a minimal terminal emulator (`MiniVt`) to catch the stale row, not just the computed frame content
- **The Settings tab's GUI-automation summary must show the real trust status, not a dead prefix match** — `gui_automation_status_lines` (`src/cli/setup_wizard/render.rs`) never emits a `"Trust status: "` prefix; the compact row derives its text from the function's real first non-"Settings action: " line — `test_gui_automation_settings_summary_shows_real_status_not_generic_placeholder` in `src/cli/setup_wizard/tests.rs` (#1298) checks both a trusted and a not-trusted case against `gui_automation_status_lines`'s own output, not a mock string
- **The Finish screen's "Ready to go!" summary must list every enabled Features-section toggle, not a hardcoded subset** — `feature_toggle_states` in `src/cli/setup_wizard/render.rs` is the single list both the Settings tab's rows and `review_section_lines`'s summary build from, so a toggle added there cannot silently vanish from the pre-save review — `test_finish_screen_summary_includes_every_enabled_features_toggle`, `test_finish_screen_summary_falls_back_to_defaults_when_every_toggle_is_off` in `src/cli/setup_wizard/tests.rs` (#1299)
- **The theme selector's help text must describe the real selection style** — the #1140 style is bold bright-white text on a black background (`wizard_selected`), so `themes_section_lines` (`src/cli/setup_wizard/render.rs`) says exactly that instead of a "white background" claim the wizard has never rendered (#1300) — `test_theme_selector_help_text_matches_actual_selection_style` in `src/cli/setup_wizard/tests.rs` cross-checks the wording against the selected row's real `\x1b[1;97;40m` bytes in the same frame
- **Every wizard checkbox uses one glyph convention, `☑`/`☐`, not a mix of bracket text and an emoji** — the Local Helpers and Settings tabs previously diverged from the Models tab's own `☑`/`☐` (Local Helpers used `[x]`/`[ ]`; Settings used `✅`/`☐`, two glyph families for one on/off concept) (#1300) — `test_wizard_checkboxes_use_one_glyph_convention_across_tabs` in `src/cli/setup_wizard/tests.rs`
- **`models_section_lines`'s primary-provider description must declare a fixed row height across all four provider-description variants, for the same reason as the Local Helpers rule above** — the no-key variant is two sentences while a keyed remote provider's, ChatGPT's, and Grok subscription's are each one, and Grok's and ChatGPT's own single-sentence variants still differ in *physical* row count from each other once the raw terminal wraps Grok's longer sentence — confirmed by an exhaustive sweep of every pair of the four variants across widths 60-150: at 131 columns, switching the primary provider from a Grok subscription to ChatGPT shrinks the block by one row and, with nothing but `wizard_boxed`'s own top border right after it, strands a leftover text fragment from Grok's wrapped second row plus a duplicate of the old primary-provider row that an independent full repaint of the same frame does not show (#1305, follow-up from #1297) — `test_models_section_grok_to_chatgpt_transition_does_not_strand_a_stale_row` in `src/cli/setup_wizard/tests.rs` replays the real two-frame `WizardHost::paint` byte stream through the same `MiniVt` emulator and asserts the incrementally-blitted screen equals an independent from-scratch repaint of the same final frame
- **In the composer, Ctrl+C copies the active transcript selection; it is not the cancel/clear/exit key** — a no-op with nothing selected, and it never touches the draft or a running query. Superseded a stale, unimplemented proposal (#895, "Ctrl+C should always cancel, never clear") with what was actually requested live: matching other terminal coding agents' copy convention. `TuiRenderer::copy_active_selection_to_clipboard` (`crates/finch-tui/src/lib.rs`) reports success or failure on the status line via `set_operation_status`, since a keyboard shortcut has no other feedback — the ("Ctrl+C", ComposerShortcut) case in `test_keyboard_shortcut_table_matches_the_real_dispatch_paths` (`crates/finch-tui/src/async_input.rs`) covers all three: draft/query untouched, no-op with no selection, and a status-line report either way with one. `read_line`/`show_dialog`'s own, separate Ctrl+C SIGINT-style cancel convention (`ctrl_c_armed_at`/`ctrl_c_should_cancel`) is unchanged — only the top-level composer stopped using it.
- **An idle, empty-composer Escape press must warn before a confirming second press exits Finch, but Escape's cancel of an active query stays single-press** — Escape's own key handling in `async_input::handle_composer_shortcuts` cannot decide this itself (no active-query visibility), so an idle Escape press only sets `TuiRenderer::pending_escape_cancel`; `EventLoop::handle_escape_cancel_request` (`src/cli/repl_event/event_loop/dispatch.rs`) cancels an active query or plan/executing overlay immediately (clearing any stale `escape_idle_exit_armed_at` left over from an earlier, unrelated idle press so it cannot silently confirm a later one), and arms its own `escape_idle_exit_armed_at` only for the remaining case that would actually exit Finch, requiring a confirming second idle Escape within `ESCAPE_IDLE_EXIT_WINDOW`; `EventLoop::sync_escape_exit_hint` puts "Press Esc again to exit Finch" on the status line (#1311) — `test_idle_escape_arm_shows_exit_warning_then_confirms_on_second_press`, `test_escape_with_active_query_cancels_immediately_without_exit_warning`, `test_escape_in_plan_overlay_cancels_immediately_without_exit_warning`, `test_escape_idle_exit_arm_expires_after_window`, `test_escape_active_query_cancel_clears_a_stale_idle_arm` in `src/cli/repl_event/event_loop/tests.rs`. This is now the composer's only idle-exit gesture — Ctrl+C's equivalent (#1301) was retired when Ctrl+C became the copy key above, and `EventLoop::apply_idle_exit_hint`'s status-line slot simplified from a two-key shared owner to Escape's own `escape_idle_exit_hint_shown` bool.
- **The bottom status rule must always show the active provider/model identity, never render as a blank line of dashes** — `EventLoop::project_model_identity` (`src/cli/repl_event/event_loop/commands.rs`) takes `tui_renderer.lock().await`, never `try_lock`: `async_input::spawn_input_task`'s own periodic `tui_renderer.lock()` (`crates/finch-tui/src/async_input.rs`, held across its `crossterm::event::poll` call) can legitimately hold the mutex at the exact moment startup hydration tries to project the identity, and nothing ever retried a lost `try_lock`, so `TuiRenderer::model_identity` stayed empty — and `status_rule_line` (`crates/finch-tui/src/lib.rs`) renders an empty identity as a bare dash rule — for the rest of the session (#1318, live-reported as a blank status line; the race predates and is unrelated to #1313/#1314/#1315, none of which touch this path). `hydrate_brain_selection_sets_model_identity_despite_renderer_lock_contention` in `src/cli/repl_event/event_loop/tests.rs` forces the contention deterministically by holding the renderer lock across the whole hydrate call.
- **Pressing to start a new transcript selection over an old, finalized one must not lose the new selection** — `TuiRenderer::handle_left_press` (`crates/finch-tui/src/lib.rs`) needs a full-viewport repaint to erase the old selection's highlight wherever it lives (possibly already-scrolled-back history, outside `paint_selection_overlay`'s live-area-only reach), but `redraw_full_viewport_inner`'s own "any full repaint clears it" rule (#221) also unconditionally drops `selection_press_candidate` — deferring that repaint via `viewport_invalidated` used to let it run on the very next `TuiRenderer::render()` call, before the press was ever promoted into a selection, silently eating its own candidate (#1378, live-reported: "select some text, then try to select text again — it just unselects the existing selection and doesn't select the new stuff"). The repaint now runs synchronously inside `handle_left_press`, which restashes the press immediately afterward — the same stash-then-restore shape `autoscroll_transcript_drag` (#1237) already uses around its own mid-drag `redraw_full_viewport` call — `test_press_after_finalized_selection_survives_the_erase_redraw_and_starts_a_new_selection` in `crates/finch-tui/src/lib.rs`'s `selection_tests` module drives the real `handle_mouse(Down)` → `TuiRenderer::render()` → `handle_mouse(Drag)` sequence `async_input.rs` uses and confirms via `VtOracle` (a real VT100 parser) that the new selection's highlight actually reaches the screen.

### Request assembly

- **Summarised request prefix is byte-stable across turns, with the stable system/context prefix preceding the summary** — past `max_verbatim`, the summary is committed to a range that only moves when the window slides past it (`SummaryCache` in `src/cli/conversation_compactor.rs`), so the head of the message array is reused byte-for-byte instead of regenerated per turn — `test_summarised_request_prefix_is_byte_stable_across_turns` in `src/cli/repl_event/query_processor.rs`
- **`ConversationCompactor::summarize` prefers a configured cloud provider over the active session generator when the active one is local** — summarising conversation history ("preserve key decisions, code written, errors fixed") is harder and more nuanced than ordinary chat, so a weak local model driving the session (e.g. Gemma 2 9B) is not trusted to compress its own history; `resolve_summary_generator` in `src/cli/repl_event/llm_loop.rs` matches the active generator's name against `available_providers` to tell whether it is a local profile, and if so resolves a configured cloud profile through `ProviderResolver::resolve_entry` (the same resolver `/model` switching uses) instead. It falls back to the active generator unchanged — the pre-#1236 behaviour — when the active generator is already non-local, no cloud provider is configured, or resolving the configured one fails — `test_resolve_summary_generator_prefers_configured_cloud_provider_over_active_local_model`, `test_resolve_summary_generator_falls_back_to_active_generator_when_no_cloud_provider_configured`, `test_resolve_summary_generator_leaves_an_already_cloud_active_generator_untouched` in `src/cli/repl_event/llm_loop.rs`

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
- **`gui_click` with raw coordinates is an internal primitive, not a user-facing tool** — it must not appear in the default tool list for non-developer personas. No persona-based tool-list filtering mechanism exists yet (`config/persona.rs`'s `Persona` only shapes system-prompt tone); this bullet names a target, not a currently enforced behavior — treat it as a bug per this file's own testing rule until a filtering mechanism and test exist.
- **Accessibility permission errors must explain how to fix them** — the error message must include the exact path to grant access (`System Settings → Privacy & Security → Accessibility`).
- **GUI automation and Excel accessibility tools register unconditionally on macOS — never gated on the `gui_automation` feature flag or Accessibility permission state (issue #421)** — a tool that does not exist cannot report why it is unavailable, which made the two invariants above unreachable by construction whenever the flag was off or permission was not yet granted. `GuiClickTool`, `GuiTypeTool`, `GuiInspectTool` (`src/tools/implementations/gui.rs`) and the six `Excel*Tool`s (`src/tools/implementations/excel.rs`) register in `src/cli/repl.rs` inside one `#[cfg(target_os = "macos")]` block with no further gate; only non-macOS platforms omit them, since AppleScript and `AXIsProcessTrusted` do not exist there. The Gui* tools carry `config.features.gui_automation` as a constructor argument and consult it at execution time through `AutomationBroker`/`AutomationAvailability::unavailable_message()` (`crates/finch-runtime/src/automation.rs`), distinguishing disabled-by-configuration from permission-not-granted; the Excel tools have no feature flag of their own and already translate macOS's Automation/Accessibility denial into an actionable message via their own `osascript()` helper. `test_gui_click_reports_disabled_setting_not_absence_when_flag_is_off`, `test_gui_inspect_availability_reports_disabled_state_even_when_flag_is_off` in `src/tools/implementations/gui.rs`; `owner_repl_catalog()` in `src/cli/repl/always_allow_tests.rs` mirrors the real registration so `test_declared_effects_match_pre_refactor_classification` covers all nine tool names.

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
