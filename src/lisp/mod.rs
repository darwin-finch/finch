//! Finch Lisp source syntax support.
//!
//! The reader produces a spanned, provider-neutral syntax tree. Executable
//! semantics belong exclusively to [`crate::vm::frontend::lisp`] and the
//! shared typed VM; this module deliberately exposes no second evaluator or
//! effectful standard library.

pub mod reader;
pub mod types;

pub use types::Val;
