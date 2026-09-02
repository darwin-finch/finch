/// The Co-Forth lexer, and nothing else.
///
/// This module once held an "English as Forth" vocabulary -- a seed lexicon of
/// 392 words, each with a generated Forth body, plus an interpreter to run
/// them. The interpreter went in #294 and the unreachable remainder in #298,
/// both as dead code. The vocabulary itself is now removed too, on the owner's
/// call that per-word generated snippets were never what the idea needed:
/// English and Chinese words were meant to *be* the operations, composing into
/// sentences that evaluate, and a dictionary of one-off demonstrations is a
/// different thing that resembles it. The data is preserved on
/// `archive/word-seed-vocabulary`.
///
/// What survives is `tokenize`, which has nothing to do with any of that: it
/// gives a Forth definition its identity, via
/// `programs::forth_definition_identity`.
pub mod tokens;

pub use tokens::tokenize;
