// Context assembly for the system prompt
//
// This module collects project-level instructions (CLAUDE.md files) and other
// context that should be prepended to every conversation's system prompt.

pub mod claude_md;
pub use claude_md::collect_claude_md_context;
