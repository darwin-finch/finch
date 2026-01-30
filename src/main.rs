// Shammah - Local-first Constitutional AI Proxy
// Main entry point

use anyhow::Result;

use shammah::claude::ClaudeClient;
use shammah::cli::Repl;
use shammah::config::load_config;
use shammah::crisis::CrisisDetector;
use shammah::metrics::MetricsLogger;
use shammah::models::ThresholdRouter;
use shammah::router::Router;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_config()?;

    // Load crisis detector
    let crisis_detector = CrisisDetector::load_from_file(&config.crisis_keywords_path)?;

    // Load or create threshold router
    let models_dir = dirs::home_dir()
        .map(|home| home.join(".shammah").join("models"))
        .expect("Failed to determine home directory");
    std::fs::create_dir_all(&models_dir)?;

    let threshold_router_path = models_dir.join("threshold_router.json");
    let threshold_router = if threshold_router_path.exists() {
        match ThresholdRouter::load(&threshold_router_path) {
            Ok(router) => {
                eprintln!(
                    "✓ Loaded threshold router with {} queries",
                    router.stats().total_queries
                );
                router
            }
            Err(e) => {
                eprintln!("Warning: Failed to load threshold router: {}", e);
                eprintln!("  Creating new threshold router");
                ThresholdRouter::new()
            }
        }
    } else {
        eprintln!("Creating new threshold router");
        ThresholdRouter::new()
    };

    // Create router with threshold router
    let router = Router::new(crisis_detector, threshold_router);

    // Create Claude client
    let claude_client = ClaudeClient::new(config.api_key.clone())?;

    // Create metrics logger
    let metrics_logger = MetricsLogger::new(config.metrics_dir.clone())?;

    // Create and run REPL
    let mut repl = Repl::new(config, claude_client, router, metrics_logger);

    repl.run().await?;

    Ok(())
}
