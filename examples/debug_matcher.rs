// Debug pattern matching

use anyhow::Result;
use tracing_subscriber;

use shammah::patterns::{PatternLibrary, PatternMatcher};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library = PatternLibrary::load_from_file(&patterns_path)?;

    let pattern_matcher = PatternMatcher::new(pattern_library, 0.3);

    let test_query = "What is the golden rule?";
    println!("Testing query: {}", test_query);

    let result = pattern_matcher.find_match(test_query);

    match result {
        Some((pattern, confidence)) => {
            println!(
                "Match found: {} (confidence: {:.2})",
                pattern.id, confidence
            );
        }
        None => {
            println!("No match found");
        }
    }

    Ok(())
}
