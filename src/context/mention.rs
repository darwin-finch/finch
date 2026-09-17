//! Composer `@` mentions: project file/directory snapshots as structured context.
//!
//! `@` is a context-reference syntax. Finch resolves the selected resource and
//! attaches the snapshot itself; providers are never asked to interpret raw
//! `@path` text. Instruction-file `@path` imports in `claude_md` are unrelated.

use crate::providers::ContentBlock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Hard cap for one mentioned file. Oversized files fail rather than attach a
/// silent prefix.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Maximum files included from one directory mention.
pub const MAX_DIR_FILES: usize = 32;
/// Maximum total bytes included from one directory mention.
pub const MAX_DIR_BYTES: u64 = 128 * 1024;
/// Directory expansion depth, counting from the mentioned directory.
pub const MAX_DIR_DEPTH: usize = 8;
/// Maximum picker rows returned for one query.
pub const MAX_PICKER_ROWS: usize = 50;
/// Stop walking after this many filesystem entries.
const MAX_WALK_ENTRIES: usize = 4_000;
/// Combined attachment budget for one submitted turn.
pub const MAX_TURN_BYTES: u64 = 256 * 1024;

const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "dist",
    "build",
    ".direnv",
    ".next",
    "coverage",
];

/// File or directory mention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionKind {
    File,
    Directory,
}

impl MentionKind {
    /// Stable lowercase kind label used in speakable rows and events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

/// One picker row. Every field is speakable text; there are no coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionCandidate {
    /// File versus directory.
    pub kind: MentionKind,
    /// Project-relative path using `/` separators.
    pub relative_path: String,
    /// Size in bytes when known (files). Directories omit this.
    pub estimated_bytes: Option<u64>,
}

impl MentionCandidate {
    /// Screen-reader row: kind, path, and size or directory budget.
    pub fn speakable_row(&self) -> String {
        match self.kind {
            MentionKind::File => {
                let size = self
                    .estimated_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "size unknown".to_string());
                format!("file {} {size}", self.relative_path)
            }
            MentionKind::Directory => format!(
                "directory {} bounded ≤{MAX_DIR_FILES} files / {}",
                self.relative_path,
                format_bytes(MAX_DIR_BYTES)
            ),
        }
    }

    /// Visible composer token inserted on select.
    pub fn insert_token(&self) -> String {
        mention_token(&self.relative_path)
    }
}

/// Bytes snapshotted at selection or submit time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionSnapshot {
    /// Project-relative path using `/` separators.
    pub relative_path: String,
    /// File versus directory expansion.
    pub kind: MentionKind,
    /// Hex SHA-256 of the attached UTF-8 payload.
    pub sha256: String,
    /// Attached payload size in bytes.
    pub byte_len: u64,
    /// True when a directory expansion named a budget truncation.
    pub truncated: bool,
    /// Speakable truncation or skip note, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_note: Option<String>,
    /// Exact attached UTF-8 contents.
    pub content: String,
}

/// One mention token parsed from visible prompt text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMention {
    /// Byte offset of the `@` in the prompt.
    pub at_offset: usize,
    /// Project-relative path decoded from the token.
    pub relative_path: String,
}

/// Borrowed attachment fields for the structured provider document.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentBody<'a> {
    /// Project-relative path.
    pub relative_path: &'a str,
    /// File versus directory.
    pub kind: MentionKind,
    /// Hex digest of the attached payload.
    pub sha256: &'a str,
    /// Attached payload size.
    pub byte_len: u64,
    /// Whether a named truncation applies.
    pub truncated: bool,
    /// Optional truncation/skip note.
    pub truncation_note: Option<&'a str>,
    /// Exact attached UTF-8 contents.
    pub content: &'a str,
}

impl MentionSnapshot {
    /// View used to format the provider-independent attachment document.
    pub fn as_body(&self) -> AttachmentBody<'_> {
        AttachmentBody {
            relative_path: &self.relative_path,
            kind: self.kind,
            sha256: &self.sha256,
            byte_len: self.byte_len,
            truncated: self.truncated,
            truncation_note: self.truncation_note.as_deref(),
            content: &self.content,
        }
    }
}

/// Why a mention could not be attached. The speakable form names the resource
/// and the failed rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionError {
    /// Path does not exist under the project root.
    Missing {
        /// Mentioned path.
        path: String,
    },
    /// Open or read failed.
    Unreadable {
        /// Mentioned path.
        path: String,
        /// OS or decoder reason.
        reason: String,
    },
    /// NUL byte or invalid UTF-8.
    Binary {
        /// Mentioned path.
        path: String,
    },
    /// File larger than [`MAX_FILE_BYTES`].
    Oversized {
        /// Mentioned path.
        path: String,
        /// Observed size.
        bytes: u64,
        /// Cap that failed.
        cap: u64,
    },
    /// Matched an ignore rule.
    Ignored {
        /// Mentioned path.
        path: String,
    },
    /// Hidden path while the query did not start with `.`.
    Hidden {
        /// Mentioned path.
        path: String,
    },
    /// Likely-secret basename.
    Secret {
        /// Mentioned path.
        path: String,
        /// Named heuristic.
        rule: String,
    },
    /// Symlink target leaves the project root.
    SymlinkEscape {
        /// Mentioned path.
        path: String,
        /// Resolved target.
        target: String,
    },
    /// Path resolved outside the project root.
    OutsideRoot {
        /// Mentioned path.
        path: String,
    },
    /// Combined turn attachments exceed [`MAX_TURN_BYTES`].
    TurnBudget {
        /// Mention that crossed the cap.
        path: String,
        /// Running total.
        bytes: u64,
        /// Cap that failed.
        cap: u64,
    },
}

impl MentionError {
    /// Fully speakable diagnostic. Names the resource and the rule.
    pub fn speakable(&self) -> String {
        match self {
            Self::Missing { path } => {
                format!("Mention `{path}` was not found under the project root.")
            }
            Self::Unreadable { path, reason } => {
                format!("Mention `{path}` could not be read ({reason}).")
            }
            Self::Binary { path } => {
                format!("Mention `{path}` is binary (NUL or non-UTF-8) and was not attached.")
            }
            Self::Oversized { path, bytes, cap } => format!(
                "Mention `{path}` is {} and exceeds the {} per-file budget; nothing was attached.",
                format_bytes(*bytes),
                format_bytes(*cap)
            ),
            Self::Ignored { path } => {
                format!("Mention `{path}` is ignored by project ignore rules and was not attached.")
            }
            Self::Hidden { path } => {
                format!("Mention `{path}` is hidden and was not attached.")
            }
            Self::Secret { path, rule } => {
                format!("Mention `{path}` looks like a secret ({rule}) and was not attached.")
            }
            Self::SymlinkEscape { path, target } => format!(
                "Mention `{path}` is a symlink to `{target}`, which is outside the project root, and was not attached."
            ),
            Self::OutsideRoot { path } => {
                format!("Mention `{path}` is outside the project root and was not attached.")
            }
            Self::TurnBudget { path, bytes, cap } => format!(
                "Mention `{path}` would bring attached context to {} over the {} turn budget; nothing was attached.",
                format_bytes(*bytes),
                format_bytes(*cap)
            ),
        }
    }
}

impl std::fmt::Display for MentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.speakable())
    }
}

impl std::error::Error for MentionError {}

/// Project-rooted mention listing and resolution.
#[derive(Debug, Clone)]
pub struct MentionCatalog {
    root: PathBuf,
}

impl MentionCatalog {
    /// Catalog rooted at `root`. The path is stored as given; listing canonicalizes
    /// when the directory exists.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Project root used for relative mention paths.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Keyboard-picker rows matching `query` (case-insensitive path substring).
    pub fn candidates(&self, query: &str) -> Vec<MentionCandidate> {
        let query = normalize_query(query);
        let show_hidden = query.starts_with('.');
        let mut scored = Vec::new();
        let walker = match mention_walker(&self.root) {
            Some(w) => w,
            None => return Vec::new(),
        };
        let mut seen = 0usize;
        for entry in walker {
            if seen >= MAX_WALK_ENTRIES || scored.len() >= MAX_PICKER_ROWS * 4 {
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            seen += 1;
            let path = entry.path();
            if path == self.root {
                continue;
            }
            let Some(relative) = relative_utf8(&self.root, path) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            if is_skip_dir_component(&relative) {
                continue;
            }
            if !show_hidden && path_is_hidden(&relative) {
                continue;
            }
            if secret_rule(&relative).is_some() {
                continue;
            }
            if let Some(score) = match_score(&relative, &query) {
                let file_type = entry.file_type();
                let is_dir = file_type.map(|t| t.is_dir()).unwrap_or(false);
                let is_file = file_type.map(|t| t.is_file()).unwrap_or(false);
                if !is_dir && !is_file {
                    continue;
                }
                if is_dir && dir_name_skipped(path) {
                    continue;
                }
                scored.push((
                    score,
                    relative.len(),
                    MentionCandidate {
                        kind: if is_dir {
                            MentionKind::Directory
                        } else {
                            MentionKind::File
                        },
                        relative_path: relative,
                        estimated_bytes: if is_file {
                            entry.metadata().ok().map(|m| m.len())
                        } else {
                            None
                        },
                    },
                ));
            }
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.relative_path.cmp(&b.2.relative_path))
        });
        scored
            .into_iter()
            .map(|(_, _, candidate)| candidate)
            .take(MAX_PICKER_ROWS)
            .collect()
    }

    /// Snapshot `relative` without rereading a prior snapshot.
    pub fn resolve_path(&self, relative: &str) -> Result<MentionSnapshot, MentionError> {
        let relative = normalize_relative(relative)?;
        if let Some(rule) = secret_rule(&relative) {
            return Err(MentionError::Secret {
                path: relative,
                rule: rule.to_string(),
            });
        }
        let abs = self.absolute_in_root(&relative)?;
        let link_meta = fs::symlink_metadata(&abs).map_err(|error| map_io(&relative, error))?;
        if link_meta.file_type().is_symlink() {
            self.ensure_symlink_stays(&abs, &relative)?;
        }
        let meta = fs::metadata(&abs).map_err(|error| map_io(&relative, error))?;
        if meta.is_dir() {
            self.resolve_directory(&relative, &abs)
        } else if meta.is_file() {
            self.resolve_file(&relative, &abs, meta.len())
        } else {
            Err(MentionError::Unreadable {
                path: relative,
                reason: "not a regular file or directory".to_string(),
            })
        }
    }

    fn absolute_in_root(&self, relative: &str) -> Result<PathBuf, MentionError> {
        if relative.is_empty() || relative == "." {
            return Ok(self.root.clone());
        }
        let mut joined = self.root.clone();
        for component in relative.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".." {
                return Err(MentionError::OutsideRoot {
                    path: relative.to_string(),
                });
            }
            joined.push(component);
        }
        Ok(joined)
    }

    fn ensure_symlink_stays(&self, path: &Path, relative: &str) -> Result<(), MentionError> {
        let target = fs::canonicalize(path).map_err(|error| map_io(relative, error))?;
        let root = fs::canonicalize(&self.root).map_err(|error| MentionError::Unreadable {
            path: relative.to_string(),
            reason: error.to_string(),
        })?;
        if !target.starts_with(&root) {
            return Err(MentionError::SymlinkEscape {
                path: relative.to_string(),
                target: target.display().to_string(),
            });
        }
        Ok(())
    }

    fn resolve_file(
        &self,
        relative: &str,
        abs: &Path,
        size: u64,
    ) -> Result<MentionSnapshot, MentionError> {
        if path_is_ignored(&self.root, abs) {
            return Err(MentionError::Ignored {
                path: relative.to_string(),
            });
        }
        if size > MAX_FILE_BYTES {
            return Err(MentionError::Oversized {
                path: relative.to_string(),
                bytes: size,
                cap: MAX_FILE_BYTES,
            });
        }
        let bytes =
            read_file_bytes(abs, MAX_FILE_BYTES).map_err(|error| map_io(relative, error))?;
        if bytes.len() as u64 > MAX_FILE_BYTES || (size > MAX_FILE_BYTES) {
            return Err(MentionError::Oversized {
                path: relative.to_string(),
                bytes: size.max(bytes.len() as u64),
                cap: MAX_FILE_BYTES,
            });
        }
        if is_binary(&bytes) {
            return Err(MentionError::Binary {
                path: relative.to_string(),
            });
        }
        let content = String::from_utf8(bytes).map_err(|_| MentionError::Binary {
            path: relative.to_string(),
        })?;
        Ok(snapshot(relative, MentionKind::File, content, false, None))
    }

    fn resolve_directory(
        &self,
        relative: &str,
        abs: &Path,
    ) -> Result<MentionSnapshot, MentionError> {
        if path_is_ignored(&self.root, abs) && abs != self.root {
            return Err(MentionError::Ignored {
                path: relative.to_string(),
            });
        }
        let walker = mention_walker(abs).ok_or_else(|| MentionError::Unreadable {
            path: relative.to_string(),
            reason: "could not walk directory".to_string(),
        })?;
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        let mut total_bytes = 0u64;
        let mut seen = 0usize;
        let mut walk_capped = false;
        for entry in walker {
            if seen >= MAX_WALK_ENTRIES {
                walk_capped = true;
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    skipped.push(format!("unreadable entry ({error})"));
                    continue;
                }
            };
            seen += 1;
            let path = entry.path();
            if path == abs {
                continue;
            }
            let Some(child_rel) = relative_utf8(&self.root, path) else {
                continue;
            };
            if is_skip_dir_component(&child_rel) || path_is_hidden(&child_rel) {
                continue;
            }
            if let Some(rule) = secret_rule(&child_rel) {
                skipped.push(format!("{child_rel} secret ({rule})"));
                continue;
            }
            let depth = child_rel
                .trim_start_matches(relative)
                .trim_start_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .count();
            if depth > MAX_DIR_DEPTH {
                continue;
            }
            let file_type = match entry.file_type() {
                Some(file_type) => file_type,
                None => continue,
            };
            if file_type.is_symlink() {
                match self.ensure_symlink_stays(path, &child_rel) {
                    Ok(()) => {}
                    Err(MentionError::SymlinkEscape { path, target }) => {
                        skipped.push(format!("{path} symlink escapes to {target}"));
                        continue;
                    }
                    Err(_) => continue,
                }
            }
            if !file_type.is_file() {
                continue;
            }
            match self.resolve_file(
                &child_rel,
                path,
                entry.metadata().map(|m| m.len()).unwrap_or(0),
            ) {
                Ok(file) => {
                    if files.len() >= MAX_DIR_FILES || total_bytes + file.byte_len > MAX_DIR_BYTES {
                        let note = format!(
                            "included {} of the directory's files because of the directory mention budget ({MAX_DIR_FILES} files / {}).",
                            files.len(),
                            format_bytes(MAX_DIR_BYTES)
                        );
                        let content = directory_payload(relative, &files, &skipped, Some(&note));
                        return Ok(snapshot(
                            relative,
                            MentionKind::Directory,
                            content,
                            true,
                            Some(note),
                        ));
                    }
                    total_bytes += file.byte_len;
                    files.push(file);
                }
                Err(MentionError::Binary { path }) => skipped.push(format!("{path} binary")),
                Err(MentionError::Oversized { path, bytes, cap }) => skipped.push(format!(
                    "{path} oversized ({} > {})",
                    format_bytes(bytes),
                    format_bytes(cap)
                )),
                Err(MentionError::Ignored { path }) => skipped.push(format!("{path} ignored")),
                Err(other) => skipped.push(other.speakable()),
            }
        }
        files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        let truncation = if walk_capped {
            Some(format!(
                "directory walk stopped after {MAX_WALK_ENTRIES} entries; included {} file(s).",
                files.len()
            ))
        } else {
            None
        };
        let content = directory_payload(relative, &files, &skipped, truncation.as_deref());
        let note = match (truncation, skipped.is_empty()) {
            (Some(note), _) => Some(note),
            (None, false) => Some(format!("skipped {}", skipped.join("; "))),
            (None, true) => None,
        };
        Ok(snapshot(
            relative,
            MentionKind::Directory,
            content,
            walk_capped,
            note,
        ))
    }
}

fn snapshot(
    relative: &str,
    kind: MentionKind,
    content: String,
    truncated: bool,
    truncation_note: Option<String>,
) -> MentionSnapshot {
    let sha256 = hex_sha256(content.as_bytes());
    let byte_len = content.len() as u64;
    MentionSnapshot {
        relative_path: relative.to_string(),
        kind,
        sha256,
        byte_len,
        truncated,
        truncation_note,
        content,
    }
}

fn directory_payload(
    relative: &str,
    files: &[MentionSnapshot],
    skipped: &[String],
    truncation: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "directory {relative}: {} file(s), {}\n",
        files.len(),
        format_bytes(files.iter().map(|f| f.byte_len).sum())
    ));
    if let Some(note) = truncation {
        out.push_str("truncation: ");
        out.push_str(note);
        out.push('\n');
    }
    if !skipped.is_empty() {
        out.push_str("skipped: ");
        out.push_str(&skipped.join("; "));
        out.push('\n');
    }
    for file in files {
        out.push_str(&format!(
            "\n--- file: {} (sha256: {}, {}) ---\n{}",
            file.relative_path,
            file.sha256,
            format_bytes(file.byte_len),
            file.content
        ));
        if !file.content.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn mention_walker(root: &Path) -> Option<ignore::Walk> {
    if !root.exists() {
        return None;
    }
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .standard_filters(false)
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .ignore(true)
        .parents(false)
        .max_depth(Some(MAX_DIR_DEPTH.saturating_add(2)));
    Some(builder.build())
}

fn path_is_ignored(root: &Path, path: &Path) -> bool {
    if path == root {
        return false;
    }
    let rel = match relative_utf8(root, path) {
        Some(rel) => rel,
        None => return false,
    };
    if is_skip_dir_component(&rel) {
        return true;
    }
    gitignore_matches(root, &rel, path.is_dir())
}

fn gitignore_matches(root: &Path, relative: &str, is_dir: bool) -> bool {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    let gi = root.join(".gitignore");
    let ig = root.join(".ignore");
    if gi.is_file() {
        let _ = builder.add(gi);
    }
    if ig.is_file() {
        let _ = builder.add(ig);
    }
    let Ok(gitignore) = builder.build() else {
        return false;
    };
    gitignore
        .matched_path_or_any_parents(relative, is_dir)
        .is_ignore()
}

fn map_io(relative: &str, error: io::Error) -> MentionError {
    if error.kind() == io::ErrorKind::NotFound {
        MentionError::Missing {
            path: relative.to_string(),
        }
    } else {
        MentionError::Unreadable {
            path: relative.to_string(),
            reason: error.to_string(),
        }
    }
}

fn read_file_bytes(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    file.take(cap + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn relative_utf8(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn normalize_relative(relative: &str) -> Result<String, MentionError> {
    let trimmed = relative.trim().trim_start_matches("./");
    let trimmed = trimmed.replace('\\', "/");
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.starts_with('/') || trimmed.contains(':') {
        return Err(MentionError::OutsideRoot {
            path: relative.to_string(),
        });
    }
    let mut parts = Vec::new();
    for part in trimmed.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(MentionError::OutsideRoot {
                path: relative.to_string(),
            });
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn normalize_query(query: &str) -> String {
    let query = query.trim();
    let query = query.strip_prefix("./").unwrap_or(query);
    query.trim_matches('"').to_string()
}

fn match_score(path: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(path.matches('/').count() as u32);
    }
    let path_l = path.to_lowercase();
    let query_l = query.to_lowercase();
    let name = path_l.rsplit('/').next().unwrap_or(&path_l);
    if name == query_l {
        Some(0)
    } else if name.starts_with(&query_l) {
        Some(1)
    } else if path_l.starts_with(&query_l) {
        Some(2)
    } else {
        path_l.find(&query_l).map(|idx| 10 + idx as u32)
    }
}

fn path_is_hidden(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| part.starts_with('.') && part != "." && part != "..")
}

fn is_skip_dir_component(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| SKIP_DIR_NAMES.contains(&part))
}

fn dir_name_skipped(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| SKIP_DIR_NAMES.contains(&name))
}

fn secret_rule(relative: &str) -> Option<&'static str> {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let lower = name.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") {
        return Some("dotenv file");
    }
    if matches!(
        lower.as_str(),
        ".netrc" | "_netrc" | ".npmrc" | ".pypirc" | "credentials.json" | "secrets.json"
    ) {
        return Some("credential file");
    }
    if matches!(
        lower.as_str(),
        "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
    ) {
        return Some("private key");
    }
    if let Some(ext) = lower.rsplit('.').next() {
        if matches!(
            ext,
            "pem" | "key" | "p12" | "pfx" | "keystore" | "jks" | "secret"
        ) {
            return Some("key material");
        }
    }
    None
}

/// Visible composer token for a relative path.
pub fn mention_token(relative_path: &str) -> String {
    let display = if relative_path.contains('/') || relative_path.starts_with('.') {
        relative_path.to_string()
    } else {
        format!("./{relative_path}")
    };
    if display
        .chars()
        .any(|c| c.is_whitespace() || c == '@' || c == '"')
    {
        format!("@\"{}\"", display.replace('"', "\\\""))
    } else {
        format!("@{display}")
    }
}

/// Active `@query` at a character cursor, if `@` is a token-boundary mention.
///
/// Returns `(byte_offset_of_at, query_after_at)`.
pub fn mention_query_at(text: &str, cursor_chars: usize) -> Option<(usize, String)> {
    let cursor_byte: usize = text.chars().take(cursor_chars).map(|c| c.len_utf8()).sum();
    if cursor_byte > text.len() {
        return None;
    }
    let before = &text[..cursor_byte];
    let at = before.rfind('@')?;
    if at > 0 {
        let prev = before[..at].chars().last()?;
        if prev == '\\' {
            return None;
        }
        if !prev.is_whitespace() {
            return None;
        }
    }
    let after = &before[at + 1..];
    if let Some(rest) = after.strip_prefix('"') {
        if rest.contains('"') {
            return None;
        }
        return Some((at, rest.to_string()));
    }
    if after.chars().any(char::is_whitespace) {
        return None;
    }
    Some((at, after.to_string()))
}

/// Mention tokens in visible prompt text. Skips `\@`, email-style `@`, and a
/// leading `@finch` addressee.
pub fn parse_visible_mentions(prompt: &str) -> Vec<ParsedMention> {
    let mut mentions = Vec::new();
    let mut chars = prompt.char_indices().peekable();
    let mut prev: Option<char> = None;
    let skip_addressee = leading_finch_addressee_end(prompt);
    while let Some((idx, ch)) = chars.next() {
        if ch == '@' {
            let escaped = prev == Some('\\');
            let boundary = prev.map(char::is_whitespace).unwrap_or(true);
            if !escaped && boundary {
                if skip_addressee.is_some_and(|end| idx < end) {
                    prev = Some(ch);
                    continue;
                }
                if let Some(path) = parse_mention_path(&prompt[idx + 1..]) {
                    if let Ok(relative) = normalize_relative(&path) {
                        if !relative.is_empty() {
                            mentions.push(ParsedMention {
                                at_offset: idx,
                                relative_path: relative,
                            });
                        }
                    }
                }
            }
        }
        prev = Some(ch);
    }
    mentions
}

fn leading_finch_addressee_end(prompt: &str) -> Option<usize> {
    let trimmed = prompt.trim_start();
    let offset = prompt.len() - trimmed.len();
    let rest = trimmed.strip_prefix("@finch")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(offset + "@finch".len())
    } else {
        None
    }
}

fn parse_mention_path(after_at: &str) -> Option<String> {
    if let Some(rest) = after_at.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            if ch == '"' {
                return Some(out);
            }
            out.push(ch);
        }
        return None;
    }
    let path: String = after_at
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Resolve prompt mentions, preferring `prior` snapshots so a later disk write
/// cannot silently replace selected bytes.
pub fn snapshots_for_prompt(
    catalog: &MentionCatalog,
    prompt: &str,
    prior: &[MentionSnapshot],
) -> Result<Vec<MentionSnapshot>, Vec<MentionError>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut used = 0u64;
    for parsed in parse_visible_mentions(prompt) {
        if let Some(existing) = prior
            .iter()
            .find(|snap| snap.relative_path == parsed.relative_path)
        {
            used = used.saturating_add(existing.byte_len);
            if used > MAX_TURN_BYTES {
                errors.push(MentionError::TurnBudget {
                    path: parsed.relative_path,
                    bytes: used,
                    cap: MAX_TURN_BYTES,
                });
            } else {
                out.push(existing.clone());
            }
            continue;
        }
        match catalog.resolve_path(&parsed.relative_path) {
            Ok(snap) => {
                used = used.saturating_add(snap.byte_len);
                if used > MAX_TURN_BYTES {
                    errors.push(MentionError::TurnBudget {
                        path: parsed.relative_path,
                        bytes: used,
                        cap: MAX_TURN_BYTES,
                    });
                } else {
                    out.push(snap);
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Provider-independent attached-context document.
pub fn format_attachment_document(items: &[AttachmentBody<'_>]) -> String {
    let mut out = String::from(
        "Attached context (resolved by Finch; the previous text is the user's visible prompt)\n",
    );
    for item in items {
        out.push('\n');
        out.push_str(&format!(
            "### {} {}\nSHA-256: {}\nBytes: {}\n",
            item.kind.as_str(),
            item.relative_path,
            item.sha256,
            item.byte_len
        ));
        if item.truncated {
            out.push_str("Truncated: yes\n");
        }
        if let Some(note) = item.truncation_note {
            out.push_str("Note: ");
            out.push_str(note);
            out.push('\n');
        }
        out.push_str("```\n");
        out.push_str(item.content);
        if !item.content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out
}

/// Visible prompt as the first text block; resolved snapshots follow.
pub fn assemble_user_content(
    visible_prompt: &str,
    snapshots: &[MentionSnapshot],
) -> Vec<ContentBlock> {
    let mut blocks = vec![ContentBlock::Text {
        text: visible_prompt.to_string(),
    }];
    if !snapshots.is_empty() {
        let bodies: Vec<AttachmentBody<'_>> =
            snapshots.iter().map(MentionSnapshot::as_body).collect();
        blocks.push(ContentBlock::Text {
            text: format_attachment_document(&bodies),
        });
    }
    blocks
}

/// One-shot query path: keep the visible prompt, then the attachment document.
pub fn assemble_query_text(visible_prompt: &str, snapshots: &[MentionSnapshot]) -> String {
    if snapshots.is_empty() {
        return visible_prompt.to_string();
    }
    let bodies: Vec<AttachmentBody<'_>> = snapshots.iter().map(MentionSnapshot::as_body).collect();
    format!(
        "{visible_prompt}\n\n{}",
        format_attachment_document(&bodies)
    )
}

/// Resolve mentions in a noninteractive query, or return speakable diagnostics.
pub fn prepare_prompt_for_query(
    root: &Path,
    prompt: &str,
) -> Result<(String, Vec<MentionSnapshot>), String> {
    let catalog = MentionCatalog::new(root);
    match snapshots_for_prompt(&catalog, prompt, &[]) {
        Ok(snapshots) => Ok((assemble_query_text(prompt, &snapshots), snapshots)),
        Err(errors) => Err(errors
            .iter()
            .map(MentionError::speakable)
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn test_token_boundary_at_opens_query_and_email_does_not() {
        assert_eq!(mention_query_at("@foo", 4), Some((0, "foo".into())));
        assert_eq!(mention_query_at("see @bar", 8), Some((4, "bar".into())));
        assert_eq!(mention_query_at("user@example.com", 16), None);
        assert_eq!(mention_query_at("\\@foo", 5), None);
        assert_eq!(mention_query_at("see\\@foo", 8), None);
    }

    #[test]
    fn test_parse_skips_email_literal_and_leading_finch_addressee() {
        let mentions = parse_visible_mentions("user@example.com and \\@not-a-mention");
        assert!(
            mentions.is_empty(),
            "email and escaped @ must stay ordinary prompt text, got {mentions:?}"
        );
        let mentions = parse_visible_mentions("@finch look at @src/foo.rs");
        assert_eq!(
            mentions
                .iter()
                .map(|m| m.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/foo.rs"],
            "leading @finch addressee is not a file mention: {mentions:?}"
        );
        let mentions = parse_visible_mentions("@\"my file.rs\" and @./plain");
        assert_eq!(
            mentions
                .iter()
                .map(|m| m.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["my file.rs", "plain"]
        );
    }

    #[test]
    fn test_select_file_attaches_exact_bytes_and_keeps_visible_prompt() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/foo.rs", "fn answer() { 42 }\n");
        let catalog = MentionCatalog::new(tmp.path());
        let snap = catalog.resolve_path("src/foo.rs").unwrap();
        assert_eq!(snap.content, "fn answer() { 42 }\n");
        let prompt = "explain @src/foo.rs please";
        let blocks = assemble_user_content(prompt, &[snap.clone()]);
        assert_eq!(
            blocks[0].as_text(),
            Some(prompt),
            "visible prompt must remain intact, got {:?}",
            blocks[0]
        );
        let attached = blocks[1].as_text().unwrap();
        assert!(
            attached.contains("fn answer() { 42 }"),
            "provider request must receive the exact selected content: {attached}"
        );
        assert!(
            attached.contains(&snap.sha256),
            "digest must be named: {attached}"
        );
    }

    #[test]
    fn test_unicode_spaces_and_at_in_paths() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "docs/my file.rs", "spaces");
        write_file(tmp.path(), "docs/cafés.rs", "unicode");
        write_file(tmp.path(), "docs/weird@name.rs", "at-sign");
        let catalog = MentionCatalog::new(tmp.path());
        let space = catalog.resolve_path("docs/my file.rs").unwrap();
        assert_eq!(space.content, "spaces");
        assert_eq!(
            MentionCandidate {
                kind: MentionKind::File,
                relative_path: "docs/my file.rs".into(),
                estimated_bytes: Some(6),
            }
            .insert_token(),
            "@\"docs/my file.rs\""
        );
        assert_eq!(
            catalog.resolve_path("docs/cafés.rs").unwrap().content,
            "unicode"
        );
        let at = catalog.resolve_path("docs/weird@name.rs").unwrap();
        assert_eq!(at.content, "at-sign");
        assert_eq!(
            mention_token("docs/weird@name.rs"),
            "@\"docs/weird@name.rs\""
        );
        let prompt = "see @\"docs/my file.rs\" and @\"docs/weird@name.rs\"";
        let parsed: Vec<_> = parse_visible_mentions(prompt)
            .into_iter()
            .map(|m| m.relative_path)
            .collect();
        assert_eq!(parsed, vec!["docs/my file.rs", "docs/weird@name.rs"]);
    }

    #[test]
    fn test_changed_file_does_not_replace_selected_snapshot() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "note.txt", "original");
        let catalog = MentionCatalog::new(tmp.path());
        let selected = catalog.resolve_path("note.txt").unwrap();
        write_file(tmp.path(), "note.txt", "changed-on-disk");
        let resolved = snapshots_for_prompt(
            &catalog,
            "read @./note.txt",
            std::slice::from_ref(&selected),
        )
        .unwrap();
        assert_eq!(
            resolved[0].content, "original",
            "submit must keep the selection snapshot, not reread disk: {:?}",
            resolved[0]
        );
        assert_eq!(resolved[0].sha256, selected.sha256);
        let reread = catalog.resolve_path("note.txt").unwrap();
        assert_eq!(reread.content, "changed-on-disk");
        assert_ne!(reread.sha256, selected.sha256);
    }

    #[test]
    fn test_missing_binary_ignored_secret_and_symlink_escape_fail_without_attachment() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "ok.txt", "hello");
        write_file(tmp.path(), "binary.bin", "a\0b");
        write_file(tmp.path(), ".env", "SECRET=1");
        write_file(
            tmp.path(),
            "id_ed25519",
            "-----BEGIN OPENSSH PRIVATE KEY-----\n",
        );
        write_file(tmp.path(), ".gitignore", "ignored.txt\n");
        write_file(tmp.path(), "ignored.txt", "nope");
        #[cfg(unix)]
        let outside = TempDir::new().unwrap();
        #[cfg(unix)]
        {
            write_file(outside.path(), "escape.txt", "escaped");
            std::os::unix::fs::symlink(
                outside.path().join("escape.txt"),
                tmp.path().join("link.txt"),
            )
            .unwrap();
        }
        let catalog = MentionCatalog::new(tmp.path());

        assert!(matches!(
            catalog.resolve_path("missing.txt"),
            Err(MentionError::Missing { .. })
        ));
        assert!(matches!(
            catalog.resolve_path("binary.bin"),
            Err(MentionError::Binary { .. })
        ));
        assert!(matches!(
            catalog.resolve_path("ignored.txt"),
            Err(MentionError::Ignored { .. })
        ));
        assert!(matches!(
            catalog.resolve_path(".env"),
            Err(MentionError::Secret { .. })
        ));
        assert!(matches!(
            catalog.resolve_path("id_ed25519"),
            Err(MentionError::Secret { .. })
        ));
        #[cfg(unix)]
        assert!(matches!(
            catalog.resolve_path("link.txt"),
            Err(MentionError::SymlinkEscape { .. })
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                tmp.path().join("ok.txt"),
                tmp.path().join("inside_link.txt"),
            )
            .unwrap();
            let inside = catalog.resolve_path("inside_link.txt").unwrap();
            assert_eq!(
                inside.content, "hello",
                "in-root symlinks must attach the target bytes, got {:?}",
                inside
            );
        }

        let err = snapshots_for_prompt(&catalog, "use @binary.bin", &[]).unwrap_err();
        assert!(
            err.iter().all(|e| matches!(e, MentionError::Binary { .. })),
            "failed mentions must not produce a partial attachment: {err:?}"
        );
    }

    #[test]
    fn test_oversized_file_fails_and_directory_budget_names_truncation() {
        let tmp = TempDir::new().unwrap();
        let big = "x".repeat((MAX_FILE_BYTES as usize) + 8);
        write_file(tmp.path(), "big.txt", &big);
        let catalog = MentionCatalog::new(tmp.path());
        match catalog.resolve_path("big.txt") {
            Err(MentionError::Oversized { bytes, cap, .. }) => {
                assert!(bytes > cap, "oversized diagnostic must name the failed cap");
            }
            other => panic!("expected oversized, got {other:?}"),
        }

        for i in 0..(MAX_DIR_FILES + 4) {
            write_file(tmp.path(), &format!("tree/f{i:02}.txt"), "hello-dir\n");
        }
        let dir = catalog.resolve_path("tree").unwrap();
        assert!(
            dir.truncated,
            "directory budget exhaustion must name truncation, got note={:?}",
            dir.truncation_note
        );
        let note = dir.truncation_note.expect("truncation note");
        assert!(
            note.contains(&MAX_DIR_FILES.to_string()),
            "truncation must name the file cap: {note}"
        );
        assert!(
            dir.content.contains("truncation:"),
            "attachment must not hide a partial directory: {}",
            dir.content
        );
    }

    #[test]
    fn test_picker_rows_are_speakable_and_skip_ignored() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", "fn main() {}");
        write_file(tmp.path(), "src/lib.rs", "pub fn lib() {}");
        write_file(tmp.path(), ".gitignore", "secret.log\n");
        write_file(tmp.path(), "secret.log", "nope");
        write_file(tmp.path(), "target/out.rs", "built");
        let catalog = MentionCatalog::new(tmp.path());
        let rows = catalog.candidates("main");
        assert!(
            rows.iter().any(|r| r.relative_path == "src/main.rs"),
            "expected src/main.rs in {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r.relative_path != "secret.log"),
            "ignored files must not appear: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r.relative_path != "target/out.rs"),
            "generated target/ must not appear: {rows:?}"
        );
        let row = rows
            .iter()
            .find(|r| r.relative_path == "src/main.rs")
            .unwrap();
        let speak = row.speakable_row();
        assert!(speak.contains("file"), "{speak}");
        assert!(speak.contains("src/main.rs"), "{speak}");
        assert!(
            !speak.contains("x=") && !speak.contains("y="),
            "rows must not use coordinates: {speak}"
        );
    }

    #[test]
    fn test_ambiguous_prefix_lists_each_match() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/util.rs", "a");
        write_file(tmp.path(), "src/utils.rs", "b");
        let catalog = MentionCatalog::new(tmp.path());
        let rows = catalog.candidates("util");
        let paths: Vec<_> = rows.iter().map(|r| r.relative_path.as_str()).collect();
        assert!(paths.contains(&"src/util.rs"), "{paths:?}");
        assert!(paths.contains(&"src/utils.rs"), "{paths:?}");
    }
}
