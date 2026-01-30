// Configuration structs

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Claude API key
    pub api_key: String,

    /// Path to patterns.json
    pub patterns_path: PathBuf,

    /// Path to crisis_keywords.json
    pub crisis_keywords_path: PathBuf,

    /// Directory for metrics storage
    pub metrics_dir: PathBuf,

    /// Similarity threshold for pattern matching (default: 0.2)
    pub similarity_threshold: f64,
}

impl Config {
    pub fn new(api_key: String) -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let project_dir = std::env::current_dir().expect("Could not determine current directory");

        Self {
            api_key,
            patterns_path: project_dir.join("data/patterns.json"),
            crisis_keywords_path: project_dir.join("data/crisis_keywords.json"),
            metrics_dir: home.join(".local/share/shammah/metrics"),
            similarity_threshold: 0.2,
        }
    }
}
