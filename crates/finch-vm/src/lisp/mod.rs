//! Finch Lisp source syntax support.
//!
//! The reader produces a spanned, provider-neutral syntax tree. Executable
//! semantics belong exclusively to the Lisp frontend in this crate and the
//! shared typed VM; this module deliberately exposes no second evaluator or
//! effectful standard library.

mod reader;
mod types;

pub use reader::{parse_math, parse_str, parse_str_spanned, SpannedVal};
pub use types::Val;
