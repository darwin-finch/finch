// Example: Simple query routing demonstration

use anyhow::Result;

use shammah::crisis::CrisisDetector;
use shammah::router::{RouteDecision, Router};

#[tokio::main]
async fn main() -> Result<()> {
    println!("Shammah - Simple Query Example\n");

    // Load crisis detector
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector = CrisisDetector::load_from_file(&crisis_path)?;
    println!("Loaded crisis detector\n");

    // Create router (patterns removed - all queries forward except crisis)
    let router = Router::new(crisis_detector);

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
                pattern_id,
                confidence,
            } => {
                println!(
                    "  → LOCAL (pattern: {}, confidence: {:.2}) [UNUSED - patterns removed]\n",
                    pattern_id, confidence
                );
            }
            RouteDecision::Forward { reason } => {
                println!("  → FORWARD (reason: {})\n", reason.as_str());
            }
        }
    }

    Ok(())
}
