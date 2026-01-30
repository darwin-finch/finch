// Pattern library loader

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub name: String,
    pub keywords: Vec<String>,
    pub template_response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternLibrary {
    pub patterns: Vec<Pattern>,
}

impl PatternLibrary {
    /// Load patterns from a JSON file
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read patterns file: {}", path.display()))?;

        let library: PatternLibrary =
            serde_json::from_str(&contents).context("Failed to parse patterns.json")?;

        Ok(library)
    }

    /// Get a pattern by ID
    pub fn get_pattern(&self, id: &str) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    /// Get all pattern IDs
    pub fn pattern_ids(&self) -> Vec<String> {
        self.patterns.iter().map(|p| p.id.clone()).collect()
    }
}
