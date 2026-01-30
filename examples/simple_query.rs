// Example: Simple query routing demonstration

use anyhow::Result;

use shammah::crisis::CrisisDetector;
use shammah::patterns::{PatternLibrary, PatternMatcher};
use shammah::router::{RouteDecision, Router};

#[tokio::main]
async fn main() -> Result<()> {
    println!("Shammah - Simple Query Example\n");

    // Load pattern library
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library = PatternLibrary::load_from_file(&patterns_path)?;
    println!("Loaded {} patterns", pattern_library.patterns.len());

    // Create pattern matcher
    let pattern_matcher = PatternMatcher::new(pattern_library.clone(), 0.2);

    // Load crisis detector
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector = CrisisDetector::load_from_file(&crisis_path)?;
    println!("Loaded crisis detector\n");

    // Create router
    let router = Router::new(pattern_matcher, crisis_detector);

    // Test queries
    let test_queries = vec![
        "What is the golden rule?",
        "Why do lies require more lies?",
        "How does trauma affect people?",
        "I'm thinking about suicide",
        "How do I learn Rust?",
    ];

    for query in test_queries {
        println!("Query: {}", query);

        let decision = router.route(query);

        match decision {
            RouteDecision::Local {
                pattern,
                confidence,
            } => {
                println!(
                    "  → LOCAL (pattern: {}, confidence: {:.2})\n",
                    pattern.id, confidence
                );
            }
            RouteDecision::Forward { reason } => {
                println!("  → FORWARD (reason: {})\n", reason.as_str());
            }
        }
    }

    Ok(())
}
