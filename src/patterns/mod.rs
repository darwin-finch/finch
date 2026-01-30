// Pattern matching module
// Public interface for constitutional pattern matching

mod library;
mod matcher;

pub use library::{Pattern, PatternLibrary};
pub use matcher::PatternMatcher;
