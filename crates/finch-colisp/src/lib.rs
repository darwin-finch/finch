//! CoLisp reader and compiler for Finch's shared typed stack IR.
//!
//! This unpublished frontend crate owns CoLisp source syntax and translation
//! into the shared semantic-construction protocol. Runtime execution remains
//! behind the `finch-vm` facade.

mod frontend;
mod lexicon;
mod reader;
mod types;

pub use frontend::{compile_lisp, compile_lisp_with_functions};
pub use lexicon::{lisp_atom_delimiter, lisp_lexicon, LispLexicon};
pub use reader::{parse_math, parse_str, parse_str_spanned, SpannedVal};
pub use types::Val;
