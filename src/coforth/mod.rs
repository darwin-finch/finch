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
/// - `Library::all_words` / `all_entries` — the whole vocabulary, for `finch library`.
///
/// The interpreter this vocabulary was written for is gone (#294). A `Forth`
/// VM *was* constructed in non-test code, by a `LazyLock` this change removed
/// along with it, but nothing outside `#[cfg(test)]` ever forced it, and every typed-program entry
/// point dispatches to `crate::runtime`. Reachable-by-accident is not the same
/// as unreachable, which is the argument for deleting it rather than
/// documenting it. What remains is the vocabulary itself, which
/// is live, and the lexer, whose token stream gives a Forth definition its
/// identity.
pub mod library;
pub mod tokens;

pub use library::{Library, WordEntry};
pub use tokens::tokenize;
