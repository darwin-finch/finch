// Phase 2 Example: End-to-end training with ModelRouter
//
// This example shows how the complete Phase 2 system works:
// 1. Router decides: forward or try local
// 2. If local: Generate → Validate → Return or fallback to Claude
// 3. If forward: Get Claude response
// 4. Learn from the interaction (online learning)
//
// Run with: cargo run --example phase2_training

use anyhow::Result;
use shammah::models::ModelConfig;
use shammah::router::ModelRouter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("Phase 2 Training Example");
    println!("========================\n");

    // Create model router with smaller config for faster demo
    let config = ModelConfig {
        vocab_size: 5000, // Smaller vocab
        hidden_dim: 128,  // Smaller model
        num_layers: 2,    // Fewer layers
        num_heads: 4,
        max_seq_len: 256,
        dropout: 0.0,
        device_preference: shammah::models::DevicePreference::Auto,
    };

    let mut router = ModelRouter::with_config(config)?;
    println!("✓ Model router created (cold start mode)\n");

    // Simulate a few queries
    let queries = vec![
        "What is Rust?",
        "How do I use lifetimes?",
        "Explain ownership in Rust",
        "What is a closure?",
        "How do I handle errors?",
    ];

    for (i, query) in queries.iter().enumerate() {
        println!("Query {}: {}", i + 1, query);
        println!("{}", "-".repeat(60));

        // Get routing decision
        let decision = router.route(query)?;
        println!("Decision: {:?}", decision);

        // For this demo, we'll simulate the flow without actually calling Claude API
        // In production, you would:
        // 1. If Forward: call Claude API
        // 2. If Local: generate locally, validate, possibly fallback to Claude

        // Simulate Claude response (in production, this would come from API)
        let claude_response = format!(
            "This is a simulated response about {}. In production, this would come from Claude API.",
            query
        );

        println!("Response: {}", claude_response);

        // Learn from this interaction (online learning)
        println!("Learning from interaction...");
        router.learn_from_claude(query, &claude_response, true)?;

        let (query_count, lr) = router.stats();
        println!("Stats: {} queries processed, LR: {}", query_count, lr);
        println!();
    }

    println!("\nTraining Summary");
    println!("================");
    let (query_count, _lr) = router.stats();
    println!("Total queries processed: {}", query_count);
    println!("Models have been updated {} times", query_count);
    println!("\nNote: In cold start (first 50 queries), all queries are forwarded.");
    println!("After query 50, the router starts making local decisions.");
    println!("After query 200, the router operates normally.");

    // Save models
    let models_dir = "/tmp/shammah-models-demo";
    println!("\nSaving models to {}...", models_dir);
    router.save_models(models_dir)?;
    println!("✓ Models saved!");

    Ok(())
}
