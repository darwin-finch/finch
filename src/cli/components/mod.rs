//! components capsule: component-owned presentation for typed messages.
//!
//! Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.
//!
//! **What this is.** The component layer of docs/TUI_DESIGN.md: the
//! presentation half of one message type. A component maintains a ViewModel
//! (retained on the message, behind the message's own lock — the ViewModel
//! and its action payloads live beside the message in `cli::messages` so
//! domain never depends upward), a chrome renderer, and subwidgets
//! constructed from the outer ViewModel each frame that choose to render or
//! not. This module proves the model with the say turn (#882, stage 1).
//!
//! **Dependency direction.** Dependencies point downward only: this module
//! depends on `cli::messages` (domain) and `crate::ui_model` (the
//! widget vocabulary), never on `cli::tui` (the engine), `crossterm`, or the
//! shadow buffer. The engine asks the `Message` trait for a component
//! snapshot and hands it here; it never matches on message type, and it
//! carries component actions opaquely — there is no central action enum.
//!
//! **Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from the
//! `pub(crate)` re-exports below; regenerate with
//! `python3 scripts/generate_interfaces.py --write` after changing them.
//!
//! **Focused tests:** `./scripts/test_brains.sh cargo test --lib --
//! cli::components::`.

mod say_turn;

pub(crate) use say_turn::card_lines;
