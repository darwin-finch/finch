# components: presentation for typed messages

This is the presentation half of one message type: a component owns a ViewModel, a renderer that
draws exactly one representation per state, and subwidgets built from that ViewModel each frame.
It exists to prove out a model where a message's rendering logic lives beside the message's domain
data rather than being reconstructed by the TUI engine from scratch every frame — the engine asks
the `Message` trait for a snapshot and hands it here, never matching on message type itself.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`docs/TUI_DESIGN.md`](../../../docs/TUI_DESIGN.md) — the design this component layer implements
(stages 1–2: the say-turn component proves the model).
