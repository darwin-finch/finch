// Configuration structs

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Claude API key
    pub api_key: String,

    /// Path to crisis_keywords.json
    pub crisis_keywords_path: PathBuf,

    /// Directory for metrics storage
    pub metrics_dir: PathBuf,
}

impl Config {
    pub fn new(api_key: String) -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let project_dir = std::env::current_dir().expect("Could not determine current directory");

        Self {
            api_key,
            crisis_keywords_path: project_dir.join("data/crisis_keywords.json"),
            metrics_dir: home.join(".shammah/metrics"),
        }
    }
}
