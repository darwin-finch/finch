//! Shared color themes and semantic schemes for Finch presentation.
//!
//! The application loads and saves the scheme; renderers convert its color
//! specifications to terminal colors. Child implementation stays private.

mod theme;

pub use theme::{
    ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageBand, MessageColors, StatusColors,
    UiColors,
};
