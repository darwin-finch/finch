//! Re-exports of the widget vocabulary.
//!
//! The vocabulary (`Rect`, `Track`, `Axis`, `Widget`, spans/styles carriers,
//! the claiming pass) lives in [`crate::ui_model`] so components
//! can build subtrees without touching `crossterm` or the shadow buffer
//! (docs/TUI_DESIGN.md, "Dependency direction"). Engine call sites keep their
//! stable `super::widgets` paths.

pub(crate) use crate::ui_model::{
    layout, natural_size, Axis, Layout, NodeLayout, Rect, Track, Widget,
};
