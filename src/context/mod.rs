// Context assembly for the system prompt and explicit turn attachments.
//
// Instruction files (AGENTS.md, CLAUDE.md, and related names) are collected into
// the system prompt. Composer `@` mentions are a separate path: they snapshot
// selected project files/directories and lower them into structured user-turn
// context. Instruction-file `@path` imports are still not expanded.

mod claude_md;
mod mention;

pub use claude_md::{
    collect_claude_md_context, collect_instructions, InstructionSource, InstructionSources,
    SourceStatus,
};
pub use mention::{
    assemble_user_content, format_attachment_document, mention_query_at, parse_visible_mentions,
    prepare_prompt_for_query, snapshots_for_prompt, AttachmentBody, MentionCandidate,
    MentionCatalog, MentionError, MentionKind, MentionSnapshot, ParsedMention, MAX_DIR_BYTES,
    MAX_DIR_FILES, MAX_FILE_BYTES,
};
