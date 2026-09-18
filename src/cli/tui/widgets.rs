//! Re-exports of the widget vocabulary.
//!
//! The vocabulary (`Rect`, `Track`, `Axis`, `Widget`, spans/styles carriers,
//! the claiming pass) moved to [`crate::cli::components::vocab`] so components
//! can build subtrees without touching `crossterm` or the shadow buffer
//! (docs/TUI_DESIGN.md, "Dependency direction"). Engine call sites keep their
//! stable `super::widgets` paths.

pub(crate) use crate::cli::components::vocab::{
    layout, natural_size, Axis, Layout, NodeLayout, Rect, Track, Widget,
};
