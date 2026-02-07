// Model Download Demo
// Demonstrates model selection and download infrastructure

use shammah::models::{ModelDownloader, ModelSelector, QwenSize};

fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Qwen Model Download Demo ===\n");

    // Step 1: Detect system RAM and select appropriate model
    println!("Step 1: Model Selection");
    println!("------------------------");
    let selected_model = ModelSelector::select_model_for_system()?;
    println!("Selected model: {}", selected_model.description());
    println!("  Model ID: {}", selected_model.model_id());
    println!(
        "  RAM requirement: {}GB",
        selected_model.ram_requirement_gb()
    );
    println!(
        "  Download size: {:.1}GB\n",
        selected_model.download_size_gb()
    );

    // Step 2: Create downloader
    println!("Step 2: Download Infrastructure");
    println!("-------------------------------");
    let downloader = ModelDownloader::new()?;
    println!("Cache directory: {:?}\n", downloader.cache_dir());

    // Step 3: Check if model is cached
    println!("Step 3: Cache Check");
    println!("-------------------");
    let is_cached = downloader.is_cached(selected_model);
    if is_cached {
        println!("✓ Model is already cached");
        println!("  No download needed - ready to use!\n");
    } else {
        println!("✗ Model not cached");
        println!(
            "  First run will download ~{:.1}GB",
            selected_model.download_size_gb()
        );
        println!("  Subsequent runs will load from cache\n");
    }

    // Step 4: Demonstrate manual override
    println!("Step 4: Manual Override Example");
    println!("--------------------------------");
    let override_model = QwenSize::Qwen1_5B; // Force smallest model
    println!("Overriding to: {}", override_model.description());

    let selected_with_override = ModelSelector::select_model_with_override(Some(override_model))?;
    println!("Selected: {}", selected_with_override.description());
    println!(
        "Is cached: {}\n",
        downloader.is_cached(selected_with_override)
    );

    // Step 5: Show all available models
    println!("Step 5: Available Models");
    println!("------------------------");
    let models = vec![
        QwenSize::Qwen1_5B,
        QwenSize::Qwen3B,
        QwenSize::Qwen7B,
        QwenSize::Qwen14B,
    ];

    for model in models {
        let cached_mark = if downloader.is_cached(model) {
            "✓"
        } else {
            "✗"
        };
        println!(
            "{} {} - {}GB RAM, {:.1}GB download",
            cached_mark,
            model.description(),
            model.ram_requirement_gb(),
            model.download_size_gb()
        );
    }

    println!("\n=== Demo Complete ===");
    println!("\nNote: Actual download would require:");
    println!("  - Network connection");
    println!("  - ~1-14GB free disk space");
    println!("  - 5-30 minutes (depending on model size and connection)");
    println!("\nTo download a model, use: cargo run --example model_download_demo -- --download");

    Ok(())
}
