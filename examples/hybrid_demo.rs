// Hybrid Router Demo - Smooth transition from threshold to neural
//
// Demonstrates the three-phase strategy:
// Phase 1 (queries 1-50): Pure threshold-based routing
// Phase 2 (queries 51-200): Hybrid with gradually increasing neural weight
// Phase 3 (queries 201+): Primarily neural with threshold safety check
//
// Run with: cargo run --example hybrid_demo

use anyhow::Result;
use shammah::router::{HybridRouter, HybridStrategy};

fn main() -> Result<()> {
    println!("Hybrid Router Demo");
    println!("==================\n");

    let mut router = HybridRouter::new()?;
    println!("✓ Created hybrid router\n");

    // Simulate queries at different stages
    let test_points = vec![
        (1, "First query"),
        (25, "Mid cold-start"),
        (50, "End of threshold-only phase"),
        (75, "Early hybrid (30% neural weight)"),
        (125, "Mid hybrid (50% neural weight)"),
        (200, "End of hybrid phase"),
        (250, "Neural primary phase"),
        (500, "Mature system"),
    ];

    println!("Strategy Evolution:\n{}", "=".repeat(70));

    for (query_num, label) in test_points {
        // Fast-forward to this query number
        while router.stats().query_count < query_num {
            router.learn_from_claude(
                &format!("Query {}", router.stats().query_count + 1),
                "Response",
                true,
            )?;
        }

        let stats = router.stats();
        let strategy = stats.strategy;

        println!("\nQuery {}: {}", query_num, label);
        match strategy {
            HybridStrategy::ThresholdOnly => {
                println!("  Strategy: THRESHOLD ONLY");
                println!("  - Using simple statistics and heuristics");
                println!("  - Immediate learning from query 1");
                println!("  - Fully interpretable decisions");
            }
            HybridStrategy::Hybrid { neural_weight } => {
                println!("  Strategy: HYBRID");
                println!(
                    "  - Threshold weight: {:.1}%",
                    (1.0 - neural_weight) * 100.0
                );
                println!("  - Neural weight: {:.1}%", neural_weight * 100.0);
                println!("  - Smooth transition in progress");
            }
            HybridStrategy::NeuralPrimary { threshold_fallback } => {
                println!("  Strategy: NEURAL PRIMARY");
                println!("  - Primarily using neural networks");
                println!(
                    "  - Threshold fallback: {}",
                    if threshold_fallback { "YES" } else { "NO" }
                );
                println!("  - Safety checks still active");
            }
        }

        // Show threshold stats
        println!("  Threshold Router:");
        println!(
            "    - Forward rate: {:.1}%",
            stats.threshold_router.forward_rate * 100.0
        );
        println!(
            "    - Confidence threshold: {:.3}",
            stats.threshold_router.confidence_threshold
        );
    }

    println!("\n\n{}", "=".repeat(70));
    println!("Key Benefits:\n");
    println!("✓ Immediate value from query 1 (threshold models)");
    println!("✓ Interpretable statistics throughout");
    println!("✓ Smooth transition to neural as data accumulates");
    println!("✓ Safety checks prevent bad decisions");
    println!("✓ Can fall back to thresholds if neural uncertain");
    println!("\nThis hybrid approach provides the best of both worlds!");

    Ok(())
}
