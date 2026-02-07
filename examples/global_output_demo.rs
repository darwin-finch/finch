// Example demonstrating the global output system (Phase 3.5)
//
// This example shows how to use the global output macros for:
// - User messages
// - Claude responses
// - Tool outputs
// - Status updates
// - Training stats
// - Download progress
//
// Run with: cargo run --example global_output_demo
// Run with logging: SHAMMAH_LOG=1 cargo run --example global_output_demo
// Run piped (non-interactive): cargo run --example global_output_demo | cat

use shammah::{
    output_claude, output_status, output_tool, output_user,
    status_download, status_operation, status_training,
};
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Global Output System Demo ===\n");

    // Check if we're in interactive mode
    let is_interactive = !shammah::cli::global_output::is_non_interactive();
    let logging_enabled = shammah::cli::global_output::logging_enabled();

    println!("Interactive mode: {}", is_interactive);
    println!("Logging enabled: {}\n", logging_enabled);

    // User message
    output_user!("What is the capital of France?");
    thread::sleep(Duration::from_millis(100));

    // Status messages (will be silent in non-interactive mode unless SHAMMAH_LOG=1)
    status_operation!("Processing query...");
    thread::sleep(Duration::from_millis(200));

    // Tool execution
    output_tool!("read", "Reading file: config.toml");
    thread::sleep(Duration::from_millis(100));

    // Claude response (prints to stdout in non-interactive mode)
    output_claude!("The capital of France is Paris. Paris is located in the north-central");
    output_claude!("part of France and has been the country's capital since the 12th century.");
    thread::sleep(Duration::from_millis(100));

    // Training stats (silent in non-interactive mode unless SHAMMAH_LOG=1)
    status_training!(42, 0.38, 0.82);
    thread::sleep(Duration::from_millis(100));

    // Download progress (silent in non-interactive mode unless SHAMMAH_LOG=1)
    // downloaded and total are in bytes
    status_download!("Qwen-2.5-3B", 0.80, 2_100_000_000u64, 2_600_000_000u64);
    thread::sleep(Duration::from_millis(100));

    // More status
    output_status!("Operation complete");

    // In interactive mode, print buffer contents
    if is_interactive {
        println!("\n=== Output Buffer Contents ===");
        let output = shammah::cli::global_output::global_output();
        let messages = output.get_messages();
        println!("Total messages in buffer: {}", messages.len());

        println!("\n=== Status Bar Contents ===");
        let status = shammah::cli::global_output::global_status();
        let lines = status.get_lines();
        println!("Total status lines: {}", lines.len());
        for line in lines {
            println!("{:?}: {}", line.line_type, line.content);
        }
    }

    println!("\n=== Demo Complete ===");
    println!("\nTry running:");
    println!("  cargo run --example global_output_demo");
    println!("  SHAMMAH_LOG=1 cargo run --example global_output_demo");
    println!("  cargo run --example global_output_demo | cat");
}
