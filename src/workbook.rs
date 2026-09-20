//! Compatibility facade for worksheet allocation bounds.
//!
//! The implementation is runtime-owned because typed workbook host effects
//! enforce the same bound. CLI rendering keeps this root path so the terminal
//! capsule does not acquire a direct runtime dependency.

pub(crate) use crate::runtime::{bounded_worksheet_range, MAX_WORKBOOK_CELLS};
