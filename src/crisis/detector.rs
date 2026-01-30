// Crisis keyword detector

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrisisKeywords {
    pub self_harm: Vec<String>,
    pub violence: Vec<String>,
    pub abuse: Vec<String>,
}

pub struct CrisisDetector {
    keywords: CrisisKeywords,
}

impl CrisisDetector {
    /// Load crisis keywords from a JSON file
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read crisis keywords file: {}", path.display()))?;

        let keywords: CrisisKeywords =
            serde_json::from_str(&contents).context("Failed to parse crisis_keywords.json")?;

        Ok(Self { keywords })
    }

    /// Detect if query contains crisis keywords
    /// Returns true if any crisis keyword is detected
    pub fn detect_crisis(&self, query: &str) -> bool {
        let query_lower = query.to_lowercase();

        // Check self-harm keywords
        for keyword in &self.keywords.self_harm {
            if query_lower.contains(&keyword.to_lowercase()) {
                tracing::warn!("Crisis detected: self-harm keyword '{}'", keyword);
                return true;
            }
        }

        // Check violence keywords
        for keyword in &self.keywords.violence {
            if query_lower.contains(&keyword.to_lowercase()) {
                tracing::warn!("Crisis detected: violence keyword '{}'", keyword);
                return true;
            }
        }

        // Check abuse keywords
        for keyword in &self.keywords.abuse {
            if query_lower.contains(&keyword.to_lowercase()) {
                tracing::warn!("Crisis detected: abuse keyword '{}'", keyword);
                return true;
            }
        }

        false
    }

    /// Get all keywords (for display purposes)
    pub fn all_keywords(&self) -> Vec<String> {
        let mut all = Vec::new();
        all.extend(self.keywords.self_harm.clone());
        all.extend(self.keywords.violence.clone());
        all.extend(self.keywords.abuse.clone());
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_detector() -> CrisisDetector {
        let keywords = CrisisKeywords {
            self_harm: vec!["suicide".to_string(), "kill myself".to_string()],
            violence: vec!["kill someone".to_string()],
            abuse: vec!["being abused".to_string()],
        };
        CrisisDetector { keywords }
    }

    #[test]
    fn test_crisis_detection() {
        let detector = create_test_detector();

        assert!(detector.detect_crisis("I'm thinking about suicide"));
        assert!(detector.detect_crisis("I want to kill myself"));
        assert!(!detector.detect_crisis("What is the meaning of life?"));
    }

    #[test]
    fn test_case_insensitive() {
        let detector = create_test_detector();

        assert!(detector.detect_crisis("SUICIDE"));
        assert!(detector.detect_crisis("SuIcIdE"));
    }
}
