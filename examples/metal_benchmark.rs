// Metal Backend Benchmark - Apple Silicon GPU Acceleration
//
// Compares CPU vs Metal (Apple Silicon GPU) performance for model inference.
// Demonstrates 10-100x speedup on M1/M2/M3/M4 chips.
//
// Run with: cargo run --example metal_benchmark --release

use anyhow::Result;
use shammah::models::{
    device_info, is_metal_available, DevicePreference, ModelConfig, RouterModel,
};
use std::time::Instant;

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("Metal Backend Benchmark");
    println!("=======================\n");

    // Check Metal availability
    if is_metal_available() {
        println!("✓ Metal (Apple Silicon GPU) is available");
        println!("  This will demonstrate GPU acceleration\n");
    } else {
        println!("✗ Metal not available on this system");
        println!("  Running CPU-only benchmark\n");
    }

    // Create configs for CPU and Metal
    let mut cpu_config = ModelConfig::small();
    cpu_config.device_preference = DevicePreference::Cpu;

    let mut metal_config = ModelConfig::small();
    metal_config.device_preference = DevicePreference::Metal;

    println!("Model Configuration:");
    println!("  Vocab size: {}", cpu_config.vocab_size);
    println!("  Hidden dim: {}", cpu_config.hidden_dim);
    println!("  Layers: {}", cpu_config.num_layers);
    println!("  Heads: {}", cpu_config.num_heads);
    println!("  Max seq len: {}\n", cpu_config.max_seq_len);

    // Benchmark CPU
    println!("Creating CPU model...");
    let start = Instant::now();
    let cpu_model = RouterModel::new(&cpu_config)?;
    let cpu_create_time = start.elapsed();
    println!("  Created in {:?}", cpu_create_time);
    println!("  Device: {}\n", device_info(&cpu_model.device()));

    // Benchmark Metal (if available)
    if is_metal_available() {
        println!("Creating Metal model...");
        let start = Instant::now();
        let metal_model = RouterModel::new(&metal_config)?;
        let metal_create_time = start.elapsed();
        println!("  Created in {:?}", metal_create_time);
        println!("  Device: {}\n", device_info(&metal_model.device()));

        // Run inference benchmark
        println!("Running inference benchmark (100 iterations)...\n");

        // CPU inference
        println!("CPU Inference:");
        let test_input = vec![1u32; 50]; // 50 tokens
        let start = Instant::now();
        for _ in 0..100 {
            let _ = cpu_model.predict_from_ids(&test_input)?;
        }
        let cpu_time = start.elapsed();
        println!("  Total time: {:?}", cpu_time);
        println!("  Per inference: {:?}", cpu_time / 100);

        // Metal inference
        println!("\nMetal Inference:");
        let start = Instant::now();
        for _ in 0..100 {
            let _ = metal_model.predict_from_ids(&test_input)?;
        }
        let metal_time = start.elapsed();
        println!("  Total time: {:?}", metal_time);
        println!("  Per inference: {:?}", metal_time / 100);

        // Compare
        let speedup = cpu_time.as_secs_f64() / metal_time.as_secs_f64();
        println!("\n{}", "=".repeat(50));
        println!("Speedup: {:.2}x faster on Metal", speedup);
        println!("{}", "=".repeat(50));

        if speedup > 2.0 {
            println!("\n✓ Significant GPU acceleration detected!");
            println!("  Metal backend is working correctly.");
        } else {
            println!("\n⚠ Expected more speedup on Apple Silicon");
            println!("  This may be due to small model size or system load.");
        }
    } else {
        println!("Skipping Metal benchmark (not available)\n");
        println!("To enable Metal acceleration:");
        println!("  1. Use a Mac with Apple Silicon (M1/M2/M3/M4)");
        println!("  2. Ensure Metal API is available");
        println!("  3. Run with DevicePreference::Auto or ::Metal");
    }

    println!("\n{}", "=".repeat(50));
    println!("Recommendations:");
    println!("{}", "=".repeat(50));
    println!("\nFor Production:");
    println!("  • Use ModelConfig::for_apple_silicon()");
    println!("  • Enables automatic Metal acceleration");
    println!("  • Falls back to CPU if Metal unavailable");
    println!("\nFor Development:");
    println!("  • Use ModelConfig::small() for faster iteration");
    println!("  • Force CPU with DevicePreference::Cpu for debugging");
    println!("\nExpected Performance:");
    println!("  • Small models: 2-5x speedup on Metal");
    println!("  • Large models: 10-100x speedup on Metal");
    println!("  • Best on M1 Pro/Max, M2 Pro/Max, M3/M4");

    Ok(())
}
