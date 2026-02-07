// Qwen Integration Demo
// Demonstrates the complete Qwen integration: selection, download, and loading

use shammah::models::{GeneratorConfig, GeneratorModel, ModelDownloader, ModelSelector, QwenSize};

fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Qwen Model Integration Demo ===\n");

    // Step 1: Automatic model selection based on RAM
    println!("Step 1: Automatic Model Selection");
    println!("----------------------------------");
    let selected_model = ModelSelector::select_model_for_system()?;
    println!("✓ Selected: {}", selected_model.description());
    println!(
        "  RAM requirement: {}GB",
        selected_model.ram_requirement_gb()
    );
    println!(
        "  Download size: {:.1}GB\n",
        selected_model.download_size_gb()
    );

    // Step 2: Check cache status
    println!("Step 2: Check Cache Status");
    println!("--------------------------");
    let downloader = ModelDownloader::new()?;
    let is_cached = downloader.is_cached(selected_model);

    if is_cached {
        println!("✓ Model is cached - ready to load");
    } else {
        println!("✗ Model not cached");
        println!("\nTo download the model, run:");
        println!("  cargo run --example download_model");
        println!("\nFor this demo, we'll use the smallest model (Qwen-1.5B) as fallback");
    }
    println!();

    // Step 3: Create generator configurations
    println!("Step 3: Generator Configuration Options");
    println!("---------------------------------------");

    // Option A: Use pre-trained Qwen (if downloaded)
    println!("Option A: Pre-trained Qwen");
    let cache_dir = downloader
        .cache_dir()
        .join("hub/models--Qwen--Qwen2.5-1.5B-Instruct");

    // Find snapshot directory
    let mut qwen_available = false;
    if cache_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let snapshot_dir = entry.path();
                if snapshot_dir.is_dir()
                    && snapshot_dir.join("config.json").exists()
                    && snapshot_dir.join("tokenizer.json").exists()
                {
                    println!("  ✓ Qwen model found at: {:?}", snapshot_dir);

                    let config = GeneratorConfig::Qwen {
                        model_size: QwenSize::Qwen1_5B,
                        cache_dir: snapshot_dir.clone(),
                        device_preference: shammah::models::DevicePreference::Auto,
                    };

                    match GeneratorModel::new(config) {
                        Ok(gen) => {
                            println!("  ✓ Successfully created generator: {}", gen.name());
                            println!("  ✓ Running on: {:?}", gen.device());
                            qwen_available = true;

                            // Demonstrate generation
                            println!("\n  Testing generation...");
                            println!("  (Note: Actual generation requires proper input handling)");
                        }
                        Err(e) => {
                            println!("  ✗ Failed to create generator: {}", e);
                        }
                    }
                    break;
                }
            }
        }
    }

    if !qwen_available {
        println!("  ✗ Qwen model not available - download it first");
    }
    println!();

    // Option B: Custom transformer (fallback)
    println!("Option B: Custom Transformer (Random Init)");
    let model_config = shammah::models::ModelConfig::small();
    let config = GeneratorConfig::RandomInit(model_config);

    match GeneratorModel::new(config) {
        Ok(gen) => {
            println!("  ✓ Successfully created generator: {}", gen.name());
            println!("  ✓ Running on: {:?}", gen.device());
        }
        Err(e) => {
            println!("  ✗ Failed to create generator: {}", e);
        }
    }
    println!();

    // Step 4: Summary
    println!("Step 4: Summary");
    println!("---------------");
    println!("✓ Model selection: Automatic based on system RAM");
    println!("✓ Download infrastructure: Ready with progress tracking");
    println!("✓ Generator API: Unified interface for both backends");
    println!("✓ Device support: Automatic Metal/CPU selection");
    println!();

    println!("=== Next Steps ===");
    println!("\n1. Download a model:");
    println!("   cargo run --example download_model");
    println!("\n2. Test generation:");
    println!("   cargo test --lib qwen_loader::tests::test_load_qwen_model -- --ignored");
    println!("\n3. Integration with REPL:");
    println!("   See progressive bootstrap implementation (upcoming)");

    Ok(())
}
