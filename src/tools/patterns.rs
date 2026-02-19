use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::executor::ToolSignature;

/// Type of pattern matching to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatternType {
    /// Wildcard matching with * and **
    Wildcard,
    /// Regular expression matching
    Regex,
    /// Structured pattern matching (matches command, args, dir separately)
    Structured,
}

impl Default for PatternType {
    fn default() -> Self {
        PatternType::Wildcard
    }
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
        }
    }

    /// Create a new structured pattern
    pub fn new_structured(
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

    /// Increment match count (deprecated, use record_match instead)
    #[deprecated(note = "Use record_match() instead")]
    pub fn increment_match(&mut self) {
        self.record_match();
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

        // Match directory pattern (if specified)
        if let Some(dir_pattern) = &self.dir_pattern {
            if let Some(dir) = &signature.directory {
                if !pattern_matches(dir_pattern, dir) {
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
    pub fn matches(&self, signature: &ToolSignature) -> bool {
        self.tool_name == signature.tool_name && self.signature == signature.context_key
    }

    /// Increment match count
    pub fn increment_match(&mut self) {
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

    /// Get pattern by ID
    pub fn get_pattern(&self, id: &str) -> Option<&ToolPattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    /// Get exact approval by ID
    pub fn get_exact(&self, id: &str) -> Option<&ExactApproval> {
        self.exact_approvals.iter().find(|a| a.id == id)
    }

    /// Find pattern by ID (returns index)
    pub fn find_by_id(&self, id: &str) -> Option<usize> {
        self.patterns.iter().position(|p| p.id == id)
    }

    /// Find pattern by ID (returns mutable reference)
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut ToolPattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }

    /// Get total number of patterns and approvals
    pub fn total_count(&self) -> usize {
        self.patterns.len() + self.exact_approvals.len()
    }

    /// Prune unused patterns (0 matches, older than 30 days)
    pub fn prune_unused(&mut self) -> usize {
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let original_count = self.patterns.len();

        self.patterns
            .retain(|p| p.match_count > 0 || p.created_at > cutoff);

        original_count - self.patterns.len()
    }
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
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
            command: Some("cargo".to_string()),
            args: Some("build".to_string()),
            directory: Some("/project".to_string()),
        };

        let sig3 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "npm test in /project".to_string(),
            command: Some("npm".to_string()),
            args: Some("test".to_string()),
            directory: Some("/project".to_string()),
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
            directory: None
        };

        let approval = ExactApproval::new(sig.clone());

        assert!(approval.matches(&sig));

        let different_sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
            command: None,
            args: None,
            directory: None
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
            directory: None
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
            directory: None
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
            directory: None
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build".to_string(),
            command: None,
            args: None,
            directory: None
        };

        let sig3 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo run".to_string(),
            command: None,
            args: None,
            directory: None
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
            directory: None
        };

        let sig2 = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /project/src/lib.rs".to_string(),
            command: None,
            args: None,
            directory: None
        };

        let sig3 = ToolSignature {
            tool_name: "read".to_string(),
            context_key: "reading /project/src/test.txt".to_string(),
            command: None,
            args: None,
            directory: None
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
            directory: None
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "testABC".to_string(),
            command: None,
            args: None,
            directory: None
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
}

