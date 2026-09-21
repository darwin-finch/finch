//! Compatibility path for the extracted Finch color theme crate.
//!
//! New renderer code imports `finch_theme` directly; application callers can
//! continue using `crate::theme` while composition paths migrate.

pub use finch_theme::{
    ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageBand, MessageColors, StatusColors,
    UiColors,
};
