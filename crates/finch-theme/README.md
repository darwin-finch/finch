# Finch color themes

`finch-theme` owns Finch's shared color vocabulary: named themes, semantic color roles,
user-configurable color specifications, their defaults, and conversion to ratatui colors. It
does not load configuration, decide which theme a session uses, paint terminal cells, or manage
the TUI lifecycle. Its serialized values are shared between configuration and presentation.

Two callers show the boundary:

1. The [configuration loader](../../src/config/loader.rs) reads a saved `ColorScheme` and
   supplies it to the application. Config owns file paths, overrides, and persistence; this crate
   owns the meaning, defaults, and serialized shape of the color roles.
2. The [TUI renderer](../../src/cli/tui/mod.rs) receives that scheme from the REPL, then uses
   `ColorSpec::to_color` and `ColorScheme::message_band_style` while painting status, dialogs, and
   transcript rows. The renderer owns layout and terminal writes; this crate only supplies color
   data and conversion.

Read the [agent contract](AGENTS.md) for dependency and compatibility rules. [`src/lib.rs`](src/lib.rs)
is the flat public facade, and rustdoc shows methods on its exported types.
