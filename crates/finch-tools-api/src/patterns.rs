use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::permissions::{bash_command_is_denylisted, resolve_canonical_path};
use crate::signature::ToolSignature;

/// Type of pattern matching to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum PatternType {
    /// Wildcard matching with * and **
    #[default]
    Wildcard,
    /// Regular expression matching
    Regex,
    /// Structured pattern matching (matches command, args, dir separately)
    Structured,
}

/// Kind of path argument a structured pattern admits.
///
/// `WorkspaceContained` is “any path under the workspace root”, not “any
/// string”. Legacy wildcard/regex patterns default to `Any`; the never-widen
/// gate still refuses escaped paths at match time for every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PathSlot {
    /// No structured path constraint. Never-widen still applies.
    #[default]
    Any,
    /// Any path under the workspace root.
    WorkspaceContained,
}

/// A pattern that can match multiple tool signatures using wildcards or regex
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPattern {
    pub id: String,
    pub pattern: String,
    pub tool_name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub match_count: u64,
    #[serde(default)]
    pub pattern_type: PatternType,
    #[serde(default)]
    pub last_used: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_by: Option<String>,
    /// Compiled regex (not serialized, rebuilt on load)
    #[serde(skip)]
    compiled_regex: Option<Regex>,

    // Structured pattern fields (only used when pattern_type == Structured)
    /// Pattern to match command (e.g., "cargo test", "git *", "*")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_pattern: Option<String>,
    /// Pattern to match arguments (e.g., "--release", "*", "")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_pattern: Option<String>,
    /// Pattern to match working directory (e.g., "/home/*/projects", "*")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir_pattern: Option<String>,
    /// Structured path-argument kind. Defaults to `Any` for stored JSON.
    #[serde(default)]
    pub path_slot: PathSlot,
}

impl ToolPattern {
    /// Create a new pattern with wildcard matching (default)
    pub fn new(pattern: String, tool_name: String, description: String) -> Self {
        Self::new_with_type(pattern, tool_name, description, PatternType::Wildcard)
    }

    /// Create a new pattern with explicit pattern type
    pub fn new_with_type(
        pattern: String,
        tool_name: String,
        description: String,
        pattern_type: PatternType,
    ) -> Self {
        let compiled_regex = if pattern_type == PatternType::Regex {
            Regex::new(&pattern).ok()
        } else {
            None
        };

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            pattern,
            tool_name,
            description,
            created_at: Utc::now(),
            match_count: 0,
            pattern_type,
            last_used: None,
            created_by: None,
            compiled_regex,
            command_pattern: None,
            args_pattern: None,
            dir_pattern: None,
            path_slot: PathSlot::Any,
        }
    }

    /// Try to build a `Structured` bash pattern that captures one command's
    /// fixed skeleton (program + subcommand words + literal flag names) and
    /// wildcards only the *values* of long (`--flag`) options — e.g.
    /// `gh issue create --repo owner/repo --title X --body Y` becomes
    /// `command_pattern: "gh"`, `args_pattern: "issue create --repo *
    /// --title * --body *"`.
    ///
    /// Returns `None` — the caller falls back to its existing `Wildcard`
    /// pattern generation, never a new, less-safe behaviour — whenever the
    /// command does not cleanly fit that shape:
    ///
    /// - it contains a shell operator (`;`, `|`, `>`, `<`, `&`): a template
    ///   over `bash -c '...'` is a template over an entire shell language,
    ///   not a single invocation, and is not analysable (#429);
    /// - any token outside the leading skeleton and the recognised
    ///   `--flag`/`--flag value`/`--flag=value` positions is a bare
    ///   positional argument (e.g. `cp <src> <dst>`, `rm -rf <arg>`) —
    ///   these are exactly the shapes #427/#429 call unsafe to templatize,
    ///   because the wildcarded slot could be a filesystem path with no
    ///   containment check;
    /// - nothing actually varies (a bare command, or only boolean flags) —
    ///   templating buys nothing a literal pattern doesn't already give.
    ///
    /// Short flags (`-v`, `-f`) are always treated as boolean/literal and
    /// never absorb a following token as a value, specifically so
    /// `rm -v <path>` does not get misread as "`-v` takes a value" and
    /// wildcard the path — only long (`--flag`) options can claim a value,
    /// matching the issue's own example.
    ///
    /// **What this does not do (still #429's open gap, not this issue's):**
    /// a long flag's *value* is wildcarded without checking whether it is a
    /// filesystem path — bash has no path slot and no execution-time
    /// containment exists yet (#429's bash sandbox/`is_readonly_bash`
    /// options remain undecided). This is still a strict narrowing versus
    /// today's `bash:*`-shaped default: the resulting pattern only matches
    /// the observed program + subcommand + flag skeleton, never an
    /// unrelated command, and the existing dangerous-input denylist and
    /// never-widen gates in [`ToolPattern::matches`] still apply to every
    /// match attempt regardless of pattern type.
    pub fn structured_from_bash_command(command: &str, description: String) -> Option<Self> {
        let (command_pattern, args_pattern) = bash_skeleton_pattern(command)?;

        // Self-verify: the generated pattern must match the exact command
        // it was derived from. A construction bug here must never persist
        // a pattern that doesn't even admit its own source invocation —
        // fall back to the caller's existing behaviour instead of guessing.
        let space_idx = command.find(' ')?;
        let observed_args = command[space_idx..].trim();
        if !pattern_matches(&args_pattern, observed_args) {
            return None;
        }

        let pattern = format!("cmd:{command_pattern} args:{args_pattern}");
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            pattern,
            tool_name: "bash".to_string(),
            description,
            created_at: Utc::now(),
            match_count: 0,
            pattern_type: PatternType::Structured,
            last_used: None,
            created_by: None,
            compiled_regex: None,
            command_pattern: Some(command_pattern),
            args_pattern: Some(args_pattern),
            dir_pattern: None,
            path_slot: PathSlot::Any,
        })
    }

    /// Create a new structured pattern. Crate-internal: structured patterns
    /// are constructed by the approval pipeline and tests, never by callers
    /// outside this crate.
    #[cfg(test)]
    fn new_structured(
        tool_name: String,
        description: String,
        command_pattern: Option<String>,
        args_pattern: Option<String>,
        dir_pattern: Option<String>,
    ) -> Self {
        // Build a readable pattern string for display
        let pattern = format!(
            "cmd:{} args:{} dir:{}",
            command_pattern.as_deref().unwrap_or("*"),
            args_pattern.as_deref().unwrap_or("*"),
            dir_pattern.as_deref().unwrap_or("*")
        );

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            pattern,
            tool_name,
            description,
            created_at: Utc::now(),
            match_count: 0,
            pattern_type: PatternType::Structured,
            last_used: None,
            created_by: None,
            compiled_regex: None,
            command_pattern,
            args_pattern,
            dir_pattern,
            path_slot: PathSlot::WorkspaceContained,
        }
    }

    /// Validate the pattern (check if regex compiles, etc.)
    pub fn validate(&self) -> Result<()> {
        match self.pattern_type {
            PatternType::Wildcard => {
                // Wildcards are always valid
                Ok(())
            }
            PatternType::Regex => {
                // Try to compile regex
                Regex::new(&self.pattern)
                    .with_context(|| format!("Invalid regex pattern: {}", self.pattern))?;
                Ok(())
            }
            PatternType::Structured => {
                // At least one structured field must be specified
                if self.command_pattern.is_none()
                    && self.args_pattern.is_none()
                    && self.dir_pattern.is_none()
                {
                    anyhow::bail!("Structured pattern must specify at least one field (command, args, or directory)");
                }
                Ok(())
            }
        }
    }

    /// Check if this pattern matches the given signature
    pub fn matches(&self, signature: &ToolSignature) -> bool {
        // Tool name must match
        if self.tool_name != signature.tool_name {
            return false;
        }

        // Never widen authority: a pattern cannot admit a command the
        // one-shot path would Deny, or an escaped path the one-shot path
        // would AskUser.
        if !pattern_may_admit(signature) {
            return false;
        }

        if self.path_slot == PathSlot::WorkspaceContained && !signature.path_in_workspace {
            return false;
        }

        // Match pattern against context_key based on type
        match self.pattern_type {
            PatternType::Wildcard => pattern_matches(&self.pattern, &signature.context_key),
            PatternType::Structured => self.matches_structured(signature),
            PatternType::Regex => {
                // Use compiled regex if available, otherwise compile on demand
                if let Some(ref regex) = self.compiled_regex {
                    regex.is_match(&signature.context_key)
                } else if let Ok(regex) = Regex::new(&self.pattern) {
                    regex.is_match(&signature.context_key)
                } else {
                    false
                }
            }
        }
    }

    /// Record a match (increment count and update last_used timestamp)
    pub fn record_match(&mut self) {
        self.match_count += 1;
        self.last_used = Some(Utc::now());
    }

    /// Ensure compiled regex is available (call after deserialization)
    fn ensure_compiled_regex(&mut self) {
        if self.pattern_type == PatternType::Regex && self.compiled_regex.is_none() {
            self.compiled_regex = Regex::new(&self.pattern).ok();
        }
    }

    /// Match using structured pattern (command, args, directory separately)
    fn matches_structured(&self, signature: &ToolSignature) -> bool {
        // Match command pattern (if specified)
        if let Some(cmd_pattern) = &self.command_pattern {
            if let Some(cmd) = &signature.command {
                if !pattern_matches(cmd_pattern, cmd) {
                    return false;
                }
            } else {
                // Pattern expects command but signature has none
                return false;
            }
        }

        // Match args pattern (if specified)
        if let Some(args_pattern) = &self.args_pattern {
            if let Some(args) = &signature.args {
                if !pattern_matches(args_pattern, args) {
                    return false;
                }
            } else if args_pattern != "*" && !args_pattern.is_empty() {
                // Pattern expects args but signature has none (unless pattern is wildcard)
                return false;
            }
        }

        // Match directory pattern (if specified) against a canonicalised path
        if let Some(dir_pattern) = &self.dir_pattern {
            if let Some(dir) = &signature.directory {
                let canonical = canonical_directory(dir);
                if !pattern_matches(dir_pattern, &canonical) {
                    return false;
                }
            } else {
                // Pattern expects directory but signature has none
                return false;
            }
        }

        // All specified patterns matched
        true
    }
}

/// An exact approval for a specific tool signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExactApproval {
    pub id: String,
    pub signature: String,
    pub tool_name: String,
    pub created_at: DateTime<Utc>,
    pub match_count: u64,
}

impl ExactApproval {
    /// Create a new exact approval
    pub fn new(signature: ToolSignature) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            signature: signature.context_key.clone(),
            tool_name: signature.tool_name.clone(),
            created_at: Utc::now(),
            match_count: 0,
        }
    }

    /// Check if this approval matches the given signature
    fn matches(&self, signature: &ToolSignature) -> bool {
        self.tool_name == signature.tool_name && self.signature == signature.context_key
    }

    /// Increment match count
    fn increment_match(&mut self) {
        self.match_count += 1;
    }
}

/// Type of match found
#[derive(Debug, Clone, PartialEq)]
pub enum MatchType {
    Exact(String),   // ID of exact approval
    Pattern(String), // ID of pattern that matched
}

/// Persistent storage for patterns and exact approvals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentPatternStore {
    pub version: u32,
    pub patterns: Vec<ToolPattern>,
    pub exact_approvals: Vec<ExactApproval>,
}

impl Default for PersistentPatternStore {
    fn default() -> Self {
        Self {
            version: 2,
            patterns: Vec::new(),
            exact_approvals: Vec::new(),
        }
    }
}

impl PersistentPatternStore {
    /// Load from JSON file (with automatic v1→v2 migration)
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read patterns from {}", path.display()))?;

        let mut store: Self =
            serde_json::from_str(&contents).context("Failed to parse patterns JSON")?;

        // Migrate from v1 to v2 if needed
        if store.version == 1 {
            store.version = 2;
            // All patterns get default values (PatternType::Wildcard, etc.)
            // These are already applied by serde's #[serde(default)]
        }

        // Ensure all regex patterns have compiled regex
        for pattern in &mut store.patterns {
            pattern.ensure_compiled_regex();
        }

        Ok(store)
    }

    /// Save to JSON file (atomic write)
    pub fn save(&self, path: &Path) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        // Write to temporary file
        let temp_path = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(self).context("Failed to serialize patterns")?;

        fs::write(&temp_path, json)
            .with_context(|| format!("Failed to write to {}", temp_path.display()))?;

        // Atomic rename
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "Failed to rename {} to {}",
                temp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    /// Add a new pattern
    pub fn add_pattern(&mut self, pattern: ToolPattern) {
        self.patterns.push(pattern);
    }

    /// Add a new exact approval
    pub fn add_exact(&mut self, approval: ExactApproval) {
        self.exact_approvals.push(approval);
    }

    /// Remove a pattern or approval by ID
    pub fn remove(&mut self, id: &str) -> bool {
        // Try patterns first
        if let Some(pos) = self.patterns.iter().position(|p| p.id == id) {
            self.patterns.remove(pos);
            return true;
        }

        // Try exact approvals
        if let Some(pos) = self.exact_approvals.iter().position(|a| a.id == id) {
            self.exact_approvals.remove(pos);
            return true;
        }

        false
    }

    /// Check if a signature matches any stored pattern or exact approval
    /// Returns the most specific match (exact > pattern)
    pub fn matches(&mut self, signature: &ToolSignature) -> Option<MatchType> {
        // Check exact approvals first (highest priority)
        for approval in &mut self.exact_approvals {
            if approval.matches(signature) {
                approval.increment_match();
                return Some(MatchType::Exact(approval.id.clone()));
            }
        }

        // Check patterns (lower priority, most specific first)
        let mut matches: Vec<(usize, usize)> = self
            .patterns
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if p.matches(signature) {
                    // Calculate specificity (fewer wildcards = more specific)
                    let wildcard_count = p.pattern.matches('*').count();
                    Some((i, wildcard_count))
                } else {
                    None
                }
            })
            .collect();

        // Sort by specificity (fewer wildcards first)
        matches.sort_by_key(|(_, count)| *count);

        // Return most specific match
        if let Some((index, _)) = matches.first() {
            let pattern = &mut self.patterns[*index];
            pattern.record_match();
            return Some(MatchType::Pattern(pattern.id.clone()));
        }

        None
    }

    /// Check if an exact approval exists (without incrementing count)
    pub fn has_exact(&self, signature: &ToolSignature) -> bool {
        self.exact_approvals.iter().any(|a| a.matches(signature))
    }

    /// Get pattern by ID (test-only: callers read the public `patterns` field)
    #[cfg(test)]
    fn get_pattern(&self, id: &str) -> Option<&ToolPattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    /// Find pattern by ID (returns index) (test-only)
    #[cfg(test)]
    fn find_by_id(&self, id: &str) -> Option<usize> {
        self.patterns.iter().position(|p| p.id == id)
    }

    /// Find pattern by ID (returns mutable reference) (test-only)
    #[cfg(test)]
    fn find_by_id_mut(&mut self, id: &str) -> Option<&mut ToolPattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }

    /// Get total number of patterns and approvals
    pub fn total_count(&self) -> usize {
        self.patterns.len() + self.exact_approvals.len()
    }
}

/// A pattern must not admit anything the one-shot path would Deny or would
/// AskUser solely because the path escaped the workspace.
fn pattern_may_admit(signature: &ToolSignature) -> bool {
    if signature.denylisted {
        return false;
    }
    if signature.path.is_some() && !signature.path_in_workspace {
        return false;
    }
    if let Some(command) = signature.full_command() {
        if bash_command_is_denylisted(&command) {
            return false;
        }
    }
    true
}

/// Build `(command_pattern, args_pattern)` for
/// [`ToolPattern::structured_from_bash_command`]. See that method's doc
/// comment for the full safety rationale; this function only implements the
/// shape test and the pattern construction.
fn bash_skeleton_pattern(command: &str) -> Option<(String, String)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Only a single invocation is analysable — the same reasoning
    // `is_readonly_bash` documents for its own, narrower fragment.
    if trimmed
        .chars()
        .any(|c| matches!(c, ';' | '|' | '>' | '<' | '&'))
    {
        return None;
    }

    let space_idx = trimmed.find(char::is_whitespace)?;
    let base_cmd = &trimmed[..space_idx];
    if base_cmd.is_empty() {
        return None;
    }
    let args_text = trimmed[space_idx..].trim();
    if args_text.is_empty() {
        return None;
    }

    let spans = shell_token_spans(args_text)?;
    if spans.is_empty() {
        return None;
    }

    // Leading run of non-flag tokens is the subcommand skeleton
    // ("issue create"). If there is no flag anywhere, every token is a bare
    // positional (or the whole thing is subcommand words with nothing to
    // parameterise) — bail rather than guess which ones are safe to vary.
    let mut skeleton_end = 0;
    while skeleton_end < spans.len()
        && !args_text[spans[skeleton_end].0..spans[skeleton_end].1].starts_with('-')
    {
        skeleton_end += 1;
    }
    if skeleton_end == spans.len() {
        return None;
    }

    let mut pattern = String::new();
    let mut cursor = 0usize;
    let mut saw_wildcard = false;
    let mut i = skeleton_end;
    while i < spans.len() {
        let (start, end) = spans[i];
        let token = &args_text[start..end];
        if !token.starts_with('-') {
            // A bare positional outside the skeleton/flag structure — the
            // `cp <src> <dst>` / `rm -rf <arg>` shape. Unsafe to templatize.
            return None;
        }

        if let Some(eq) = token.find('=') {
            if !token.starts_with("--") {
                // `-f=x` is nonstandard/ambiguous; don't guess.
                return None;
            }
            pattern.push_str(&args_text[cursor..start + eq + 1]);
            pattern.push('*');
            saw_wildcard = true;
            cursor = end;
            i += 1;
            continue;
        }

        if token.starts_with("--") {
            // A long flag may claim the following token as its value.
            if i + 1 < spans.len() {
                let (next_start, next_end) = spans[i + 1];
                let next = &args_text[next_start..next_end];
                if !next.starts_with('-') {
                    pattern.push_str(&args_text[cursor..end]);
                    pattern.push_str(&args_text[end..next_start]);
                    pattern.push('*');
                    saw_wildcard = true;
                    cursor = next_end;
                    i += 2;
                    continue;
                }
            }
        }
        // Short flag, or a long flag with nothing following it (or
        // followed by another flag): boolean, stays entirely literal.
        // Short flags NEVER absorb a following token as a value — that is
        // what keeps `rm -v <path>` from being misread as "`-v` takes a
        // value" and wildcarding the path.
        i += 1;
    }
    pattern.push_str(&args_text[cursor..]);

    if !saw_wildcard {
        return None;
    }

    Some((base_cmd.to_string(), pattern))
}

/// Split `s` into shell-word byte-offset spans on whitespace, treating a
/// `'...'`/`"..."` run as part of one token (so a quoted value containing a
/// space is not mistaken for two tokens). Returns `None` on an unbalanced
/// quote rather than guessing where it closes.
fn shell_token_spans(s: &str) -> Option<Vec<(usize, usize)>> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let len = s.len();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].1.is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = chars[i].0;
        let mut quote: Option<char> = None;
        while i < chars.len() {
            let ch = chars[i].1;
            if let Some(q) = quote {
                i += 1;
                if ch == q {
                    quote = None;
                }
                continue;
            }
            if ch == '\'' || ch == '"' {
                quote = Some(ch);
                i += 1;
                continue;
            }
            if ch.is_whitespace() {
                break;
            }
            i += 1;
        }
        if quote.is_some() {
            return None;
        }
        let end = if i < chars.len() { chars[i].0 } else { len };
        spans.push((start, end));
    }
    Some(spans)
}

fn canonical_directory(dir: &str) -> String {
    let path = Path::new(dir);
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    resolve_canonical_path(path, &cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string())
}

/// Match a pattern against a string using wildcards
/// Supports:
/// - `*` for single component wildcard
/// - `**` for recursive wildcard (paths)
fn pattern_matches(pattern: &str, text: &str) -> bool {
    // Handle recursive wildcard (**) in paths
    if pattern.contains("**") {
        return pattern_matches_recursive(pattern, text);
    }

    // Handle single-level wildcards (*)
    pattern_matches_simple(pattern, text)
}

/// Simple pattern matching with single-level wildcards (*)
fn pattern_matches_simple(pattern: &str, text: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('*').collect();

    // If no wildcards, must be exact match
    if pattern_parts.len() == 1 {
        return pattern == text;
    }

    let mut text_pos = 0;

    for (i, part) in pattern_parts.iter().enumerate() {
        if i == 0 {
            // First part must match at start
            if !text[text_pos..].starts_with(part) {
                return false;
            }
            text_pos += part.len();
        } else if i == pattern_parts.len() - 1 {
            // Last part must match at end
            if !text[text_pos..].ends_with(part) {
                return false;
            }
        } else {
            // Middle parts must appear in order
            if let Some(pos) = text[text_pos..].find(part) {
                text_pos += pos + part.len();
            } else {
                return false;
            }
        }
    }

    true
}

/// Pattern matching with recursive wildcards (**)
fn pattern_matches_recursive(pattern: &str, text: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split("**").collect();

    let mut text_pos = 0;

    for (i, part) in pattern_parts.iter().enumerate() {
        // Skip empty parts (from leading/trailing **)
        if part.is_empty() {
            continue;
        }

        if i == 0 {
            // First part must match at start
            if !text[text_pos..].starts_with(part) {
                return false;
            }
            text_pos += part.len();
        } else if i == pattern_parts.len() - 1 {
            // Last part must appear somewhere after current position
            if let Some(pos) = text[text_pos..].find(part) {
                text_pos += pos + part.len();
            } else {
                return false;
            }
        } else {
            // Middle parts must appear in order
            if let Some(pos) = text[text_pos..].find(part) {
                text_pos += pos + part.len();
            } else {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_matches_wildcard_command() {
        assert!(pattern_matches("cargo * in /dir", "cargo test in /dir"));
        assert!(pattern_matches("cargo * in /dir", "cargo build in /dir"));
        assert!(!pattern_matches("cargo * in /dir", "npm test in /dir"));
        assert!(!pattern_matches("cargo * in /dir", "cargo test in /other"));
    }

    #[test]
    fn test_pattern_matches_wildcard_directory() {
        assert!(pattern_matches("cargo test in *", "cargo test in /any/dir"));
        assert!(pattern_matches("cargo test in *", "cargo test in /other"));
        assert!(!pattern_matches("cargo test in *", "cargo build in /dir"));
    }

    #[test]
    fn test_pattern_matches_both_wildcards() {
        assert!(pattern_matches("cargo * in *", "cargo test in /dir"));
        assert!(pattern_matches("cargo * in *", "cargo build in /other"));
        assert!(!pattern_matches("cargo * in *", "npm test in /dir"));
    }

    #[test]
    fn test_pattern_matches_recursive_wildcard() {
        assert!(pattern_matches(
            "reading /project/**",
            "reading /project/src/main.rs"
        ));
        assert!(pattern_matches(
            "reading /project/**",
            "reading /project/a/b/c/file.rs"
        ));
        assert!(!pattern_matches(
            "reading /project/**",
            "reading /other/file.rs"
        ));
    }

    #[test]
    fn test_pattern_matches_exact() {
        assert!(pattern_matches("cargo test", "cargo test"));
        assert!(!pattern_matches("cargo test", "cargo build"));
    }

    #[test]
    fn test_tool_pattern_matches() {
        let pattern = ToolPattern::new(
            "cargo * in /project".to_string(),
            "bash".to_string(),
            "Test pattern".to_string(),
        );

        let sig1 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
            command: Some("cargo".to_string()),
            args: Some("test".to_string()),
            directory: Some("/project".to_string()),
            ..Default::default()
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
            command: Some("cargo".to_string()),
            args: Some("build".to_string()),
            directory: Some("/project".to_string()),
            ..Default::default()
        };

        let sig3 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "npm test in /project".to_string(),
            command: Some("npm".to_string()),
            args: Some("test".to_string()),
            directory: Some("/project".to_string()),
            ..Default::default()
        };

        assert!(pattern.matches(&sig1));
        assert!(pattern.matches(&sig2));
        assert!(!pattern.matches(&sig3));
    }

    #[test]
    fn test_exact_approval_matches() {
        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let approval = ExactApproval::new(sig.clone());

        assert!(approval.matches(&sig));

        let different_sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        assert!(!approval.matches(&different_sig));
    }

    #[test]
    fn test_persistent_store_priority() {
        let mut store = PersistentPatternStore::default();

        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        // Add pattern
        let pattern = ToolPattern::new(
            "cargo * in /project".to_string(),
            "bash".to_string(),
            "Pattern".to_string(),
        );
        store.add_pattern(pattern);

        // Add exact approval
        let exact = ExactApproval::new(sig.clone());
        store.add_exact(exact);

        // Exact should take priority
        let match_result = store.matches(&sig);
        assert!(matches!(match_result, Some(MatchType::Exact(_))));
    }

    #[test]
    fn test_persistent_store_remove() {
        let mut store = PersistentPatternStore::default();

        let pattern = ToolPattern::new(
            "cargo * in /project".to_string(),
            "bash".to_string(),
            "Pattern".to_string(),
        );
        let pattern_id = pattern.id.clone();
        store.add_pattern(pattern);

        assert_eq!(store.patterns.len(), 1);
        assert!(store.remove(&pattern_id));
        assert_eq!(store.patterns.len(), 0);
    }

    #[test]
    fn test_pattern_specificity() {
        let mut store = PersistentPatternStore::default();

        // Add general pattern
        let general = ToolPattern::new(
            "cargo * in *".to_string(),
            "bash".to_string(),
            "General".to_string(),
        );
        store.add_pattern(general);

        // Add specific pattern
        let specific = ToolPattern::new(
            "cargo test in /project".to_string(),
            "bash".to_string(),
            "Specific".to_string(),
        );
        let specific_id = specific.id.clone();
        store.add_pattern(specific);

        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        // Should match specific pattern (0 wildcards) over general (2 wildcards)
        let match_result = store.matches(&sig);
        if let Some(MatchType::Pattern(id)) = match_result {
            assert_eq!(id, specific_id);
        } else {
            panic!("Expected pattern match");
        }
    }

    #[test]
    fn test_regex_pattern_basic() {
        let pattern = ToolPattern::new_with_type(
            r"^cargo (test|build)$".to_string(),
            "bash".to_string(),
            "Regex pattern".to_string(),
            PatternType::Regex,
        );

        let sig1 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let sig3 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo run".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        assert!(pattern.matches(&sig1));
        assert!(pattern.matches(&sig2));
        assert!(!pattern.matches(&sig3));
    }

    #[test]
    fn test_regex_pattern_complex() {
        let pattern = ToolPattern::new_with_type(
            r"reading /project/src/.*\.rs$".to_string(),
            "read".to_string(),
            "Match Rust source files".to_string(),
            PatternType::Regex,
        );

        let sig1 = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /project/src/main.rs".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let sig2 = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /project/src/lib.rs".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let sig3 = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /project/src/test.txt".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        assert!(pattern.matches(&sig1));
        assert!(pattern.matches(&sig2));
        assert!(!pattern.matches(&sig3));
    }

    #[test]
    fn test_pattern_validation() {
        // Valid wildcard pattern
        let wildcard = ToolPattern::new(
            "cargo * in *".to_string(),
            "bash".to_string(),
            "Test".to_string(),
        );
        assert!(wildcard.validate().is_ok());

        // Valid regex pattern
        let valid_regex = ToolPattern::new_with_type(
            r"^test\d+$".to_string(),
            "bash".to_string(),
            "Test".to_string(),
            PatternType::Regex,
        );
        assert!(valid_regex.validate().is_ok());

        // Invalid regex pattern
        let invalid_regex = ToolPattern::new_with_type(
            r"^test[".to_string(), // Unclosed bracket
            "bash".to_string(),
            "Test".to_string(),
            PatternType::Regex,
        );
        assert!(invalid_regex.validate().is_err());
    }

    #[test]
    fn test_record_match_updates_timestamp() {
        let mut pattern = ToolPattern::new(
            "cargo *".to_string(),
            "bash".to_string(),
            "Test".to_string(),
        );

        assert_eq!(pattern.match_count, 0);
        assert!(pattern.last_used.is_none());

        pattern.record_match();

        assert_eq!(pattern.match_count, 1);
        assert!(pattern.last_used.is_some());

        let first_timestamp = pattern.last_used.unwrap();

        // Wait a bit and record another match
        std::thread::sleep(std::time::Duration::from_millis(10));
        pattern.record_match();

        assert_eq!(pattern.match_count, 2);
        assert!(pattern.last_used.unwrap() > first_timestamp);
    }

    #[test]
    fn test_find_by_id() {
        let mut store = PersistentPatternStore::default();

        let pattern1 = ToolPattern::new(
            "pattern1".to_string(),
            "bash".to_string(),
            "Test1".to_string(),
        );
        let id1 = pattern1.id.clone();

        let pattern2 = ToolPattern::new(
            "pattern2".to_string(),
            "bash".to_string(),
            "Test2".to_string(),
        );
        let id2 = pattern2.id.clone();

        store.add_pattern(pattern1);
        store.add_pattern(pattern2);

        assert_eq!(store.find_by_id(&id1), Some(0));
        assert_eq!(store.find_by_id(&id2), Some(1));
        assert_eq!(store.find_by_id("nonexistent"), None);
    }

    #[test]
    fn test_find_by_id_mut() {
        let mut store = PersistentPatternStore::default();

        let pattern = ToolPattern::new(
            "pattern1".to_string(),
            "bash".to_string(),
            "Test".to_string(),
        );
        let id = pattern.id.clone();
        store.add_pattern(pattern);

        if let Some(p) = store.find_by_id_mut(&id) {
            p.description = "Updated description".to_string();
        }

        assert_eq!(
            store.get_pattern(&id).unwrap().description,
            "Updated description"
        );
    }

    #[test]
    fn test_migration_v1_to_v2() {
        use tempfile::NamedTempFile;

        // Create a v1 format JSON file
        let v1_json = r#"{
            "version": 1,
            "patterns": [
                {
                    "id": "test-id",
                    "pattern": "cargo *",
                    "tool_name": "bash",
                    "description": "Test pattern",
                    "created_at": "2026-01-30T12:00:00Z",
                    "match_count": 5
                }
            ],
            "exact_approvals": []
        }"#;

        let temp_file = NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), v1_json).unwrap();

        // Load should automatically migrate to v2
        let store = PersistentPatternStore::load(temp_file.path()).unwrap();

        assert_eq!(store.version, 2);
        assert_eq!(store.patterns.len(), 1);

        let pattern = &store.patterns[0];
        assert_eq!(pattern.id, "test-id");
        assert_eq!(pattern.pattern_type, PatternType::Wildcard);
        assert!(pattern.last_used.is_none());
        assert!(pattern.created_by.is_none());
        assert_eq!(pattern.match_count, 5);
    }

    #[test]
    fn test_regex_pattern_serialization() {
        use tempfile::NamedTempFile;

        let mut store = PersistentPatternStore::default();

        let pattern = ToolPattern::new_with_type(
            r"^test\d+$".to_string(),
            "bash".to_string(),
            "Regex pattern".to_string(),
            PatternType::Regex,
        );
        let pattern_id = pattern.id.clone();
        store.add_pattern(pattern);

        let temp_file = NamedTempFile::new().unwrap();
        store.save(temp_file.path()).unwrap();

        // Load and verify regex pattern works
        let mut loaded_store = PersistentPatternStore::load(temp_file.path()).unwrap();

        let sig1 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "test123".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "testABC".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };

        // Should match test123 but not testABC
        let result1 = loaded_store.matches(&sig1);
        assert!(matches!(result1, Some(MatchType::Pattern(_))));

        let result2 = loaded_store.matches(&sig2);
        assert!(result2.is_none());

        // Verify the pattern still exists after matching
        let loaded_pattern = loaded_store.get_pattern(&pattern_id).unwrap();
        assert_eq!(loaded_pattern.pattern_type, PatternType::Regex);
        assert_eq!(loaded_pattern.match_count, 1); // Incremented by matches()
    }

    #[test]
    fn test_created_by_field() {
        let mut pattern = ToolPattern::new_with_type(
            "test".to_string(),
            "bash".to_string(),
            "Test".to_string(),
            PatternType::Wildcard,
        );

        assert!(pattern.created_by.is_none());

        pattern.created_by = Some("user@example.com".to_string());
        assert_eq!(pattern.created_by.as_deref(), Some("user@example.com"));
    }

    // ── Regression tests for "Allow All" pattern matching ─────────────────────
    // The auto-generated pattern was previously "tool_name:*" which never matched
    // because the context_key format has no "tool_name:" prefix.
    // Now it uses "*" which correctly matches any context_key.

    #[test]
    fn test_wildcard_star_matches_any_context_key() {
        // The "Allow All" pattern: "*" on tool_name "bash" should match any bash call.
        let pattern = ToolPattern::new(
            "*".to_string(),
            "bash".to_string(),
            "Allow all bash".to_string(),
        );
        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /Users/user/repos/finch".to_string(),
            command: Some("cargo".to_string()),
            args: Some("test".to_string()),
            directory: Some("/Users/user/repos/finch".to_string()),
            ..Default::default()
        };
        assert!(
            pattern.matches(&sig),
            "\"*\" pattern must match any bash context_key"
        );
    }

    #[test]
    fn test_wildcard_star_matches_read_context_key() {
        // "read" tool context_key is "reading /path/to/file" — "*" must match.
        let pattern = ToolPattern::new(
            "*".to_string(),
            "read".to_string(),
            "Allow all reads".to_string(),
        );
        let sig = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /Users/user/repos/finch/src/main.rs".to_string(),
            command: None,
            args: None,
            directory: Some("/Users/user/repos/finch".to_string()),
            ..Default::default()
        };
        assert!(
            pattern.matches(&sig),
            "\"*\" pattern must match read's context_key"
        );
    }

    #[test]
    fn test_old_broken_pattern_does_not_match_bash() {
        // Regression: old auto-generated pattern "bash:*" must NOT match
        // bash's context_key "cargo test in /project" (no "bash:" prefix in key).
        let pattern = ToolPattern::new(
            "bash:*".to_string(),
            "bash".to_string(),
            "old broken".to_string(),
        );
        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
            command: Some("cargo".to_string()),
            args: Some("test".to_string()),
            directory: Some("/project".to_string()),
            ..Default::default()
        };
        assert!(
            !pattern.matches(&sig),
            "old \"bash:*\" pattern must NOT match bash context_key"
        );
    }

    #[test]
    fn test_old_broken_pattern_does_not_match_read() {
        // Regression: old auto-generated "read:*" must NOT match "reading /path".
        let pattern = ToolPattern::new(
            "read:*".to_string(),
            "read".to_string(),
            "old broken".to_string(),
        );
        let sig = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /Users/user/repos/finch/src/main.rs".to_string(),
            command: None,
            args: None,
            directory: Some("/Users/user/repos/finch".to_string()),
            ..Default::default()
        };
        assert!(
            !pattern.matches(&sig),
            "old \"read:*\" pattern must NOT match read context_key"
        );
    }

    #[test]
    fn test_wildcard_star_does_not_match_wrong_tool() {
        // A "*" pattern on "bash" must NOT match a "read" call (tool_name gates it).
        let pattern =
            ToolPattern::new("*".to_string(), "bash".to_string(), "bash only".to_string());
        let sig = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /some/file".to_string(),
            command: None,
            args: None,
            directory: None,
            ..Default::default()
        };
        assert!(
            !pattern.matches(&sig),
            "\"*\" on bash must NOT match a read call"
        );
    }

    #[test]
    fn test_star_pattern_does_not_match_escaped_path() {
        let pattern = ToolPattern::new(
            "*".to_string(),
            "read".to_string(),
            "Allow all reads".to_string(),
        );
        let escaped = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /etc/passwd".to_string(),
            path: Some("/etc/passwd".to_string()),
            path_in_workspace: false,
            ..Default::default()
        };
        assert!(
            !pattern.matches(&escaped),
            "invariant: a * grant must not admit an escaped path; pattern would \
             otherwise widen authority past the one-shot AskUser"
        );

        let contained = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading src/main.rs".to_string(),
            path: Some("src/main.rs".to_string()),
            path_in_workspace: true,
            ..Default::default()
        };
        assert!(
            pattern.matches(&contained),
            "control: workspace-contained paths still match *"
        );
    }

    #[test]
    fn test_structured_path_slot_is_workspace_contained() {
        let pattern = ToolPattern::new_structured(
            "read".to_string(),
            "any workspace path".to_string(),
            None,
            None,
            Some("*".to_string()),
        );
        assert_eq!(pattern.path_slot, PathSlot::WorkspaceContained);
        assert_eq!(pattern.pattern_type, PatternType::Structured);

        let contained = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading src/lib.rs".to_string(),
            directory: Some("/project".to_string()),
            path: Some("src/lib.rs".to_string()),
            path_in_workspace: true,
            ..Default::default()
        };
        assert!(
            pattern.matches(&contained),
            "structured WorkspaceContained admits a workspace path"
        );

        let escaped = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /etc/passwd".to_string(),
            directory: Some("/project".to_string()),
            path: Some("/etc/passwd".to_string()),
            path_in_workspace: false,
            ..Default::default()
        };
        assert!(
            !pattern.matches(&escaped),
            "structured WorkspaceContained must not admit an escaped path"
        );
    }

    #[test]
    fn test_pattern_does_not_admit_denylisted_bash() {
        let pattern = ToolPattern::new(
            "*".to_string(),
            "bash".to_string(),
            "allow all bash".to_string(),
        );
        let denied = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "rm -rf / in /project".to_string(),
            command: Some("rm".to_string()),
            args: Some("-rf /".to_string()),
            directory: Some("/project".to_string()),
            denylisted: true,
            ..Default::default()
        };
        assert!(
            !pattern.matches(&denied),
            "invariant: a pattern must not admit a denylisted bash command"
        );
    }

    #[test]
    fn test_path_slot_defaults_on_legacy_json() {
        let v2 = r#"{
            "version": 2,
            "patterns": [
                {
                    "id": "legacy",
                    "pattern": "*",
                    "tool_name": "read",
                    "description": "old",
                    "created_at": "2026-01-30T12:00:00Z",
                    "match_count": 0
                }
            ],
            "exact_approvals": []
        }"#;
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), v2).unwrap();
        let store = PersistentPatternStore::load(temp_file.path()).unwrap();
        assert_eq!(store.patterns[0].path_slot, PathSlot::Any);
    }

    // ── #427 item 3: structured_from_bash_command ──────────────────────────

    #[test]
    fn test_structured_from_bash_command_captures_flag_skeleton() {
        // The issue's own motivating example.
        let pattern = ToolPattern::structured_from_bash_command(
            "gh issue create --repo owner/repo --title fix --body details",
            "test".to_string(),
        )
        .expect("a flag-value shape must mint a Structured pattern");

        assert_eq!(pattern.pattern_type, PatternType::Structured);
        assert_eq!(pattern.tool_name, "bash");
        assert_eq!(pattern.command_pattern.as_deref(), Some("gh"));
        assert_eq!(
            pattern.args_pattern.as_deref(),
            Some("issue create --repo * --title * --body *")
        );

        // The pattern generated from one observation must match that same
        // observation's signature (self-consistency, not just a different
        // gh issue create invocation).
        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "irrelevant".to_string(),
            command: Some("gh".to_string()),
            args: Some("issue create --repo owner/repo --title fix --body details".to_string()),
            ..Default::default()
        };
        assert!(
            pattern.matches(&sig),
            "generated pattern must match its own source command"
        );

        // A different subcommand under the same program must NOT match —
        // the changing-subcommand collapse #427/#429 call out by name.
        let different_subcommand = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "irrelevant".to_string(),
            command: Some("gh".to_string()),
            args: Some("repo delete owner/repo --confirm".to_string()),
            ..Default::default()
        };
        assert!(
            !pattern.matches(&different_subcommand),
            "invariant: `gh issue create` and `gh repo delete` must never \
             collapse into one template"
        );

        // A same-shape call with different flag values must still match —
        // that is the entire point of templating.
        let same_shape_different_values = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "irrelevant".to_string(),
            command: Some("gh".to_string()),
            args: Some(
                "issue create --repo other/repo --title \"another bug\" --body \"more details\""
                    .to_string(),
            ),
            ..Default::default()
        };
        assert!(
            pattern.matches(&same_shape_different_values),
            "same command shape with different flag values must still match"
        );
    }

    #[test]
    fn test_structured_from_bash_command_falls_back_on_bare_positional_args() {
        // `cp <src> <dst>` — no flags at all, just positionals. Templating
        // this would wildcard an unconstrained filesystem path (#429).
        assert!(
            ToolPattern::structured_from_bash_command(
                "cp ./target/out.txt /tmp/backup.txt",
                "test".to_string(),
            )
            .is_none(),
            "a bare-positional command must not be templatized"
        );
    }

    #[test]
    fn test_structured_from_bash_command_falls_back_on_positional_after_flags() {
        // `git commit -m msg file.txt` — a positional trailing a flag's
        // value is still a positional; must not templatize.
        assert!(
            ToolPattern::structured_from_bash_command(
                "git commit -m msg file.txt",
                "test".to_string(),
            )
            .is_none(),
            "a positional argument after a flag must not be templatized"
        );
    }

    #[test]
    fn test_structured_from_bash_command_short_flag_never_absorbs_a_value() {
        // `rm -v /etc/passwd` must not be read as "-v takes a value" —
        // short flags are always boolean/literal here, so the path falls
        // through as a stray positional and the whole command is declined.
        assert!(
            ToolPattern::structured_from_bash_command("rm -v /etc/passwd", "test".to_string())
                .is_none(),
            "a short flag must never absorb a following path as its value"
        );
    }

    #[test]
    fn test_structured_from_bash_command_falls_back_on_shell_operators() {
        for command in [
            "ls foo && rm -rf bar",
            "cat file | tee out",
            "echo hi > file",
            "ls; rm -rf /",
        ] {
            assert!(
                ToolPattern::structured_from_bash_command(command, "test".to_string()).is_none(),
                "a command with a shell operator must never be templatized: {command}"
            );
        }
    }

    #[test]
    fn test_structured_from_bash_command_falls_back_when_nothing_varies() {
        // All-boolean-flags / bare commands: nothing to parameterize, so
        // templating buys nothing over the existing Wildcard/exact paths.
        for command in ["ls -la", "cargo fmt", "git status", "pwd"] {
            assert!(
                ToolPattern::structured_from_bash_command(command, "test".to_string()).is_none(),
                "a command with nothing varying must fall back: {command}"
            );
        }
    }

    #[test]
    fn test_structured_from_bash_command_supports_flag_equals_value() {
        let pattern =
            ToolPattern::structured_from_bash_command("cargo build --target=aarch64", "t".into())
                .expect("--flag=value shape must templatize");
        assert_eq!(pattern.command_pattern.as_deref(), Some("cargo"));
        assert_eq!(pattern.args_pattern.as_deref(), Some("build --target=*"));
    }

    #[test]
    fn test_structured_from_bash_command_never_admits_a_denylisted_match() {
        // Even a Structured pattern goes through the same never-widen gate
        // as Wildcard: `pattern_may_admit` still refuses a denylisted bash
        // command at match time, regardless of pattern type.
        let pattern = ToolPattern::structured_from_bash_command(
            "gh issue create --repo owner/repo --title x --body y",
            "test".to_string(),
        )
        .expect("control: this shape must templatize");
        let denied_sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "irrelevant".to_string(),
            command: Some("gh".to_string()),
            args: Some("issue create --repo owner/repo --title x --body y".to_string()),
            denylisted: true,
            ..Default::default()
        };
        assert!(
            !pattern.matches(&denied_sig),
            "invariant: Structured patterns must not admit a denylisted command either"
        );
    }
}
