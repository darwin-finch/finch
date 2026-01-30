// Threshold Models Demo - Immediate Learning Without Neural Networks
//
// This example demonstrates the threshold-based approach:
// 1. ThresholdRouter: Statistics-based routing using query categories
// 2. ThresholdValidator: Rule-based quality validation using heuristics
//
// Key advantages over neural networks:
// - Shows improvement from query 1 (no cold start period)
// - Interpretable (can see exactly why decisions are made)
// - Fast (no gradient computation)
// - Can bootstrap data for neural network training later
//
// Run with: cargo run --example threshold_demo

use anyhow::Result;
use shammah::models::{ThresholdRouter, ThresholdValidator};

fn main() -> Result<()> {
    println!("Threshold Models Demo");
    println!("=====================\n");
    println!("Demonstrating immediate learning without neural network overhead\n");

    // Create threshold router and validator
    let mut router = ThresholdRouter::new();
    let mut validator = ThresholdValidator::new();

    println!("✓ Created threshold-based router and validator\n");

    // Test queries categorized by type
    let test_queries = vec![
        // Greetings (easy to handle locally)
        ("Hello!", "Hi there! How can I help you today?"),
        ("Hi", "Hello! What can I do for you?"),
        ("Hey there", "Hey! I'm here to assist you."),

        // Definitions (pattern-based)
        ("What is Rust?", "Rust is a systems programming language focused on safety, speed, and concurrency."),
        ("What is a variable?", "A variable is a named storage location in memory that holds a value."),
        ("Who is Dennis Ritchie?", "Dennis Ritchie was the creator of the C programming language."),

        // How-to questions
        ("How do I use lifetimes?", "Lifetimes in Rust ensure references are valid. Use 'a syntax to annotate them."),
        ("How to handle errors?", "Rust uses Result<T, E> for error handling. Use ? operator or match."),
        ("How can I learn Rust?", "Start with the Rust Book, then build small projects to practice."),

        // Explanations
        ("Explain ownership", "Ownership is Rust's core feature: each value has one owner, preventing memory bugs."),
        ("Explain closures", "Closures are anonymous functions that can capture their environment."),

        // Code-related
        ("```rust\nfn main() {}\n```", "This is a basic Rust main function that serves as the entry point."),

        // Debugging
        ("Fix this error: expected ;", "You're missing a semicolon. Rust requires ; at the end of statements."),

        // Comparisons
        ("Rust vs C++", "Rust offers memory safety without garbage collection, while C++ gives more control but less safety."),

        // Complex queries (should forward)
        ("How do I implement a distributed consensus algorithm with Raft?", "Complex response about Raft..."),
        ("Explain quantum computing and its implications", "Complex response about quantum..."),
    ];

    println!("Phase 1: Cold Start (first 10 queries - everything forwards)\n");
    println!("{}", "=".repeat(70));

    for (i, (query, response)) in test_queries.iter().enumerate().take(10) {
        println!("\nQuery {}: \"{}\"", i + 1, query);

        // Router decision
        let should_try = router.should_try_local(query);
        println!(
            "  Router: {} (cold start)",
            if should_try { "TRY LOCAL" } else { "FORWARD" }
        );

        // Validator decision (if we tried local)
        let is_valid = validator.validate(query, response);
        println!(
            "  Validator: {} (cold start)",
            if is_valid { "ACCEPT" } else { "REJECT" }
        );

        // Learn from Claude response
        router.learn(query, true); // Assume Claude succeeded
        validator.learn(query, response, true); // Assume Claude gave good response

        println!("  → Learned from Claude response");
    }

    println!("\n\n{}", "=".repeat(70));
    println!("Phase 2: Warm Up (queries 11-20 - starting to see patterns)\n");
    println!("{}", "=".repeat(70));

    for (i, (query, response)) in test_queries.iter().enumerate().skip(10).take(10) {
        println!("\nQuery {}: \"{}\"", i + 1, query);

        // Router decision
        let should_try = router.should_try_local(query);
        let router_stats = router.stats();
        println!(
            "  Router: {} (confidence threshold: {:.3})",
            if should_try { "TRY LOCAL" } else { "FORWARD" },
            router_stats.confidence_threshold
        );

        // Validator decision
        let is_valid = validator.validate(query, response);
        let val_stats = validator.stats();
        println!(
            "  Validator: {} ({:.1}% approval rate)",
            if is_valid { "ACCEPT" } else { "REJECT" },
            val_stats.approval_rate * 100.0
        );

        // Simulate: if router said try local and validator accepted, it's a success
        let was_successful = should_try && is_valid;

        if was_successful {
            println!("  ✓ Handled locally!");
            router.learn(query, true);
            validator.learn(query, response, true);
        } else {
            println!("  → Forwarded to Claude");
            router.learn(query, true);
            validator.learn(query, response, true);
        }
    }

    // Show final statistics
    println!("\n\n{}", "=".repeat(70));
    println!("Final Statistics");
    println!("{}", "=".repeat(70));

    let router_stats = router.stats();
    println!("\nRouter:");
    println!("  Total queries: {}", router_stats.total_queries);
    println!("  Local attempts: {}", router_stats.total_local_attempts);
    println!("  Successes: {}", router_stats.total_successes);
    println!("  Forward rate: {:.1}%", router_stats.forward_rate * 100.0);
    println!(
        "  Success rate (local): {:.1}%",
        router_stats.success_rate * 100.0
    );
    println!(
        "  Confidence threshold: {:.3}",
        router_stats.confidence_threshold
    );
    println!("  Min samples: {}", router_stats.min_samples);

    println!("\nRouter by category:");
    for (category, stats) in &router_stats.categories {
        if stats.local_attempts > 0 {
            println!(
                "  {:?}: {} attempts, {:.1}% success",
                category,
                stats.local_attempts,
                stats.successes as f64 / stats.local_attempts as f64 * 100.0
            );
        }
    }

    let val_stats = validator.stats();
    println!("\nValidator:");
    println!("  Total validations: {}", val_stats.total_validations);
    println!("  Approved: {}", val_stats.approved);
    println!("  Rejected: {}", val_stats.rejected);
    println!("  Approval rate: {:.1}%", val_stats.approval_rate * 100.0);

    println!("\nValidator signal correlations:");
    for (signal, stats) in &val_stats.signal_stats {
        let total = stats.present_and_good + stats.present_and_bad;
        if total > 0 {
            let precision = stats.present_and_good as f64 / total as f64;
            println!(
                "  {:?}: {:.1}% precision ({} samples)",
                signal,
                precision * 100.0,
                total
            );
        }
    }

    // Save models
    let models_dir = "/tmp/shammah-threshold-models";
    std::fs::create_dir_all(models_dir)?;

    println!("\n\nSaving models to {}...", models_dir);
    router.save(format!("{}/router.json", models_dir))?;
    validator.save(format!("{}/validator.json", models_dir))?;
    println!("✓ Models saved!");

    println!("\n\nKey Insights:");
    println!("=============");
    println!("1. Router starts conservative (95% confidence threshold)");
    println!("2. After 10 queries, it begins trying categories with 3+ successes");
    println!("3. Validator rejects everything for first 10 queries (forces learning)");
    println!("4. Both adapt thresholds based on performance");
    println!("5. You can see exactly which categories work well");
    println!("\nThis provides immediate value and interpretability that neural");
    println!("networks can't match during cold start!");

    Ok(())
}
