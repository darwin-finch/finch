# context — public interface

Generated from [`src/context/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/context/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Borrowed attachment fields for the structured provider document.
pub struct AttachmentBody<'a> { … }
/// One instruction file found while collecting, in precedence order.
pub struct InstructionSource { … }
/// The instruction files found for a working directory and the text assembled from them.
pub struct InstructionSources { … }
impl InstructionSources {
    /// Paths whose contents were included, in prompt order.
    pub fn loaded(&self) -> impl Iterator<Item = &Path>;
    /// The assembled instructions, or `None` when nothing was loaded.
    pub fn text(&self) -> Option<&str>;
}
/// One picker row.
pub struct MentionCandidate { … }
impl MentionCandidate {
    /// Visible composer token inserted on select.
    pub fn insert_token(&self) -> String;
    /// Screen-reader row: kind, path, and size or directory budget.
    pub fn speakable_row(&self) -> String;
}
/// Project-rooted mention listing and resolution.
pub struct MentionCatalog { … }
impl MentionCatalog {
    /// Keyboard-picker rows matching `query` (case-insensitive path substring).
    pub fn candidates(&self, query: &str) -> Vec<MentionCandidate>;
    /// Catalog rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self;
    /// Snapshot `relative` without rereading a prior snapshot.
    pub fn resolve_path(&self, relative: &str) -> Result<MentionSnapshot, MentionError>;
    /// Project root used for relative mention paths.
    pub fn root(&self) -> &Path;
}
/// Why a mention could not be attached.
pub enum MentionError { Missing, Unreadable, Binary, Oversized, Ignored, Hidden, Secret, SymlinkEscape, OutsideRoot, TurnBudget }
impl MentionError {
    /// Fully speakable diagnostic.
    pub fn speakable(&self) -> String;
}
/// File or directory mention.
pub enum MentionKind { File, Directory }
impl MentionKind {
    /// Stable lowercase kind label used in speakable rows and events.
    pub fn as_str(self) -> &'static str;
}
/// Bytes snapshotted at selection or submit time.
pub struct MentionSnapshot { … }
impl MentionSnapshot {
    /// View used to format the provider-independent attachment document.
    pub fn as_body(&self) -> AttachmentBody<'_>;
}
/// One mention token parsed from visible prompt text.
pub struct ParsedMention { … }
/// What happened to one instruction file that exists on disk.
pub enum SourceStatus { Loaded, Empty, SupersededBy, TooLarge, Unreadable }
```

## Functions

```rust
/// Visible prompt as the first text block; resolved snapshots follow.
pub fn assemble_user_content(visible_prompt: &str, snapshots: &[MentionSnapshot]) -> Vec<ContentBlock> { … }
/// Collect the instructions visible from `cwd` using the real home directory.
pub fn collect_claude_md_context(cwd: &Path) -> Option<String> { … }
/// Collect the instructions visible from `cwd`, reading user-level files under `home`.
pub fn collect_instructions(cwd: &Path, home: Option<&Path>) -> InstructionSources { … }
/// Provider-independent attached-context document.
pub fn format_attachment_document(items: &[AttachmentBody<'_>]) -> String { … }
/// Active `@query` at a character cursor, if `@` is a token-boundary mention.
pub fn mention_query_at(text: &str, cursor_chars: usize) -> Option<(usize, String)> { … }
/// Mention tokens in visible prompt text.
pub fn parse_visible_mentions(prompt: &str) -> Vec<ParsedMention> { … }
/// Resolve prompt mentions, preferring `prior` snapshots so a later disk write cannot silently replace selected bytes.
pub fn snapshots_for_prompt(catalog: &MentionCatalog, prompt: &str, prior: &[MentionSnapshot]) -> Result<Vec<MentionSnapshot>, Vec<MentionError>> { … }
```

## Constants

```rust
/// Maximum total bytes included from one directory mention.
pub const MAX_DIR_BYTES: u64 = 128 * 1024;
/// Maximum files included from one directory mention.
pub const MAX_DIR_FILES: usize = 32;
/// Hard cap for one mentioned file.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
```

## Modules

```rust
pub mod claude_md;
pub mod mention;
```
