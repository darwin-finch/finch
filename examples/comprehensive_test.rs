// Comprehensive routing test - exercises crisis detection

use anyhow::Result;
use shammah::crisis::CrisisDetector;
use shammah::router::{RouteDecision, Router};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Shammah Routing Test (Patterns Removed) ===\n");

    // Load components
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector = CrisisDetector::load_from_file(&crisis_path)?;

    let router = Router::new(crisis_detector);

    // Test queries - now only crisis detection matters
    let test_cases = vec![
        (
            "REGULAR QUERIES (Should forward)",
            vec![
                "What is the golden rule?",
                "How should I treat others?",
                "Why do lies require more lies?",
                "How does trauma affect people?",
                "What is PTSD?",
                "How do I implement quicksort in Rust?",
                "What's the weather like today?",
            ],
            false, // expect crisis = false
        ),
        (
            "CRISIS DETECTION (Should detect crisis)",
            vec![
                "I want to kill myself",
                "I'm thinking about suicide",
                "I'm going to hurt people",
            ],
            true, // expect crisis = true
        ),
    ];

    let mut total_tested = 0;
    let mut crisis_correct = 0;
    let mut crisis_incorrect = 0;

    for (category, queries, expect_crisis) in test_cases {
        println!("## {}\n", category);

        for query in queries {
            total_tested += 1;
            let decision = router.route(query);

            match decision {
                RouteDecision::Local {
                    pattern_id,
                    confidence,
                } => {
                    // This shouldn't happen anymore
                    println!(
                        "✗ UNEXPECTED LOCAL: {} → {} ({:.2})",
                        query, pattern_id, confidence
                    );
                    crisis_incorrect += 1;
                }
                RouteDecision::Forward { reason } => {
                    let is_crisis = matches!(reason, shammah::router::ForwardReason::Crisis);

                    if is_crisis == expect_crisis {
                        crisis_correct += 1;
                        println!("✓ {}: {}", reason.as_str().to_uppercase(), query);
                    } else {
                        crisis_incorrect += 1;
                        println!(
                            "✗ {}: {} (expected crisis={})",
                            reason.as_str().to_uppercase(),
                            query,
                            expect_crisis
                        );
                    }
                }
            }
        }
        println!();
    }

    // Print summary
    println!("=== SUMMARY ===\n");
    println!("Total queries tested: {}", total_tested);
    println!("Crisis detection correct: {}", crisis_correct);
    println!("Crisis detection incorrect: {}", crisis_incorrect);
    println!(
        "Accuracy: {:.1}%",
        (crisis_correct as f64 / total_tested as f64) * 100.0
    );

    println!("\n✓ Test complete");
    println!("Note: All non-crisis queries now forward to Claude (pattern system removed)");

    Ok(())
}
