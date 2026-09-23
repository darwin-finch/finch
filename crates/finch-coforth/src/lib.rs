//! Co-Forth source compiler for Finch's shared typed VM.
//!
//! This crate owns source parsing and translation from Co-Forth syntax into
//! the shared semantic-construction protocol. It does not mint executable
//! modules except through independent verification.

mod compiler;
mod lexicon;

pub use compiler::{compile_forth, compile_forth_with_functions, read_forth_source};
pub use lexicon::{forth_lexicon, ForthLexicon, ForthStringOpener};
