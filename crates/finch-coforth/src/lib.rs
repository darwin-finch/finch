//! Co-Forth source compiler for Finch's shared typed VM.
//!
//! This crate owns source parsing, type elaboration, and lowering from
//! Co-Forth into the provider-neutral IR supplied by `finch-vm-core`.

mod compiler;

pub use compiler::{compile_forth, compile_forth_with_functions};
