# cli::components — public interface

Generated from [`src/cli/components/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/cli/components/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Functions

```rust
/// Render the say card's lines for one frame: the chrome's furniture row, the `ProgramSource` subwidget (zero rows while hidden), then the `Output` subwidget.
pub(crate) fn card_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> { … }
```

## Modules

```rust
pub(crate) mod vocab;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `RenderedTranscriptLine`
