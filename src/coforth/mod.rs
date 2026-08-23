/// Co-Forth English library — every word in the English language as a Forth word.
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
pub mod arm_vm;
pub mod co_session;
pub mod generator;
pub mod interpreter;
pub mod irc_proto;
pub mod library;
pub mod scatter;

pub use interpreter::{extract_comments, tokenize, DictionarySnapshot, Forth, StackEffect};
pub use library::{Library, WordEntry};
