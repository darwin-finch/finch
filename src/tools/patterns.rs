use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::executor::ToolSignature;

/// A pattern that can match multiple tool signatures using wildcards
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPattern {
    pub id: String,
    pub pattern: String,
    pub tool_name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub match_count: u64,
}

impl ToolPattern {
    /// Create a new pattern
    pub fn new(pattern: String, tool_name: String, description: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            pattern,
            tool_name,
            description,
            created_at: Utc::now(),
            match_count: 0,
        }
    }

    /// Check if this pattern matches the given signature
    pub fn matches(&self, signature: &ToolSignature) -> bool {
        // Tool name must match
        if self.tool_name != signature.tool_name {
            return false;
        }

        // Match pattern against context_key
        pattern_matches(&self.pattern, &signature.context_key)
    }

    /// Increment match count
    pub fn increment_match(&mut self) {
        self.match_count += 1;
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
            version: 1,
            patterns: Vec::new(),
            exact_approvals: Vec::new(),
        }
    }
}

impl PersistentPatternStore {
    /// Load from JSON file
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read patterns from {}", path.display()))?;

        let store: Self =
            serde_json::from_str(&contents).context("Failed to parse patterns JSON")?;

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
            pattern.increment_match();
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
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
        };

        let sig3 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "npm test in /project".to_string(),
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
        };

        let approval = ExactApproval::new(sig.clone());

        assert!(approval.matches(&sig));

        let different_sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build in /project".to_string(),
        };

        assert!(!approval.matches(&different_sig));
    }

    #[test]
    fn test_persistent_store_priority() {
        let mut store = PersistentPatternStore::default();

        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test in /project".to_string(),
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
        let general_id = general.id.clone();
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
        };

        // Should match specific pattern (0 wildcards) over general (2 wildcards)
        let match_result = store.matches(&sig);
        if let Some(MatchType::Pattern(id)) = match_result {
            assert_eq!(id, specific_id);
        } else {
            panic!("Expected pattern match");
        }
    }
}
