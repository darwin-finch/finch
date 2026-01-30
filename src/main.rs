// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::Result;

use shammah::claude::ClaudeClient;
use shammah::cli::Repl;
use shammah::config::load_config;
use shammah::crisis::CrisisDetector;
use shammah::metrics::MetricsLogger;
use shammah::router::Router;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_config()?;

    // Load crisis detector
    let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

    // Create router
    let router = Router::new(crisis_detector);

    // Create Claude client
    let claude_client = ClaudeClient::new(config.api_key.clone())?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create and run REPL
    let mut repl = Repl::new(
        config,
        claude_client,
        router,
        metrics_logger,
    );

    repl.run().await?;

    Ok(())
}
