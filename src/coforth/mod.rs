/// Co-Forth word library — English as Forth.
///
/// English is a very complicated Forth program.  Each word calls other words.
/// Meaning is computed by execution.  This library gives Co-Forth a base
/// vocabulary so users never start from nothing.
///
/// Architecture:
/// - `WordEntry` — a word, its definition, and its relations to other words.
/// - `Library` — the full vocabulary, loaded from embedded TOML + optional
///   user-extended `~/.finch/library.toml`.
/// - `Library::lookup` — find a word and its neighbours.
/// - `Library::related` — walk the graph N hops from a seed word.
/// - `Library::inject_into_poset` — seed a poset with a word's neighbourhood.
///
/// The interpreter this vocabulary was written for is gone (#294). No `Forth`
/// VM was ever constructed in the binary: every typed-program entry point
/// dispatches to `crate::runtime`. What remains is the vocabulary itself, which
/// is live, and the lexer, whose token stream gives a Forth definition its
/// identity.
pub mod library;
pub mod tokens;

pub use library::{Library, WordEntry};
pub use tokens::tokenize;
