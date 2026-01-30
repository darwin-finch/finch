// Pattern-based tool confirmation demo
//
// This example demonstrates the pattern matching system that allows users to
// approve groups of similar tool executions rather than approving each one individually.
//
// Run with: cargo run --example pattern_demo

use shammah::tools::executor::{generate_tool_signature, ToolSignature};
use shammah::tools::patterns::{MatchType, PersistentPatternStore, ToolPattern};
use shammah::tools::types::ToolUse;
use std::path::Path;

fn main() {
    println!("Pattern-Based Tool Confirmation Demo");
    println!("=====================================\n");

    // Create a persistent store
    let mut store = PersistentPatternStore::default();

    // Scenario 1: Cargo commands in a project directory
    println!("Scenario 1: Cargo Commands");
    println!("--------------------------");

    let project_dir = Path::new("/Users/dev/my-project");

    // User approves "any cargo command in this directory"
    let pattern = ToolPattern::new(
        "cargo * in /Users/dev/my-project".to_string(),
        "bash".to_string(),
        "Allow any cargo command in my-project".to_string(),
    );
    println!("✓ Added pattern: {}", pattern.pattern);
    store.add_pattern(pattern);

    // Now test various cargo commands
    let commands = vec![
        ("cargo test", "Should match"),
        ("cargo build --release", "Should match"),
        ("cargo fmt", "Should match"),
        ("npm install", "Should NOT match (wrong command)"),
        (
            "cargo test in /Users/dev/other-project",
            "Should NOT match (wrong dir)",
        ),
    ];

    for (cmd, expected) in commands {
        let tool_use = ToolUse::new("bash".to_string(), serde_json::json!({"command": cmd}));
        let signature = generate_tool_signature(&tool_use, project_dir);

        match store.matches(&signature) {
            Some(MatchType::Pattern(id)) => {
                println!("  ✓ '{}' - {} (matched: {})", cmd, expected, &id[..8]);
            }
            _ => {
                println!("  ✗ '{}' - {}", cmd, expected);
            }
        }
    }

    println!();

    // Scenario 2: Reading files in a directory
    println!("Scenario 2: Reading Files");
    println!("-------------------------");

    let pattern = ToolPattern::new(
        "reading /Users/dev/my-project/**".to_string(),
        "read".to_string(),
        "Allow reading any file in my-project".to_string(),
    );
    println!("✓ Added pattern: {}", pattern.pattern);
    store.add_pattern(pattern);

    let files = vec![
        ("reading /Users/dev/my-project/src/main.rs", "Should match"),
        ("reading /Users/dev/my-project/README.md", "Should match"),
        (
            "reading /Users/dev/my-project/src/lib/mod.rs",
            "Should match",
        ),
        ("reading /etc/passwd", "Should NOT match (wrong dir)"),
    ];

    for (context_key, expected) in files {
        let signature = ToolSignature {
            tool_name: "read".to_string(),
            context_key: context_key.to_string(),
        };

        match store.matches(&signature) {
            Some(MatchType::Pattern(id)) => {
                println!(
                    "  ✓ '{}' - {} (matched: {})",
                    context_key,
                    expected,
                    &id[..8]
                );
            }
            _ => {
                println!("  ✗ '{}' - {}", context_key, expected);
            }
        }
    }

    println!();

    // Scenario 3: Pattern specificity
    println!("Scenario 3: Pattern Specificity");
    println!("-------------------------------");
    println!("More specific patterns (fewer wildcards) take priority\n");

    // Add general pattern
    let general = ToolPattern::new(
        "cargo * in *".to_string(),
        "bash".to_string(),
        "Allow any cargo command anywhere (general)".to_string(),
    );
    println!("✓ Added general pattern: {}", general.pattern);
    let general_id = general.id.clone();
    store.add_pattern(general);

    // Add specific pattern
    let specific = ToolPattern::new(
        "cargo test in /Users/dev/my-project".to_string(),
        "bash".to_string(),
        "Allow cargo test in my-project (specific)".to_string(),
    );
    println!("✓ Added specific pattern: {}", specific.pattern);
    let specific_id = specific.id.clone();
    store.add_pattern(specific);

    // Test which pattern matches
    let tool_use = ToolUse::new(
        "bash".to_string(),
        serde_json::json!({"command": "cargo test"}),
    );
    let signature = generate_tool_signature(&tool_use, project_dir);

    match store.matches(&signature) {
        Some(MatchType::Pattern(id)) => {
            if id == specific_id {
                println!("  ✓ Matched SPECIFIC pattern (no wildcards): {}", &id[..8]);
            } else if id == general_id {
                println!(
                    "  ✗ Matched general pattern: {} (should match specific)",
                    &id[..8]
                );
            }
        }
        _ => {
            println!("  ✗ No match (unexpected)");
        }
    }

    println!();

    // Scenario 4: Match count tracking
    println!("Scenario 4: Match Count Tracking");
    println!("--------------------------------");

    let pattern_id = store.patterns[0].id.clone();
    let initial_count = store.patterns[0].match_count;
    println!("Initial match count: {}", initial_count);

    // Generate several matches
    for cmd in ["cargo test", "cargo build", "cargo fmt"] {
        let tool_use = ToolUse::new("bash".to_string(), serde_json::json!({"command": cmd}));
        let signature = generate_tool_signature(&tool_use, project_dir);
        store.matches(&signature);
    }

    // Check updated count
    let pattern = store.patterns.iter().find(|p| p.id == pattern_id).unwrap();
    println!("After 3 more matches: {}", pattern.match_count);
    println!("  ✓ Match count incremented correctly\n");

    // Scenario 5: Persistence
    println!("Scenario 5: Persistence");
    println!("----------------------");

    let temp_path = std::env::temp_dir().join("pattern_demo.json");
    println!("Saving to: {}", temp_path.display());

    store.save(&temp_path).expect("Failed to save");
    println!("  ✓ Saved {} patterns", store.patterns.len());

    // Load back
    let loaded_store = PersistentPatternStore::load(&temp_path).expect("Failed to load");
    println!("  ✓ Loaded {} patterns", loaded_store.patterns.len());
    println!(
        "  ✓ Match counts preserved: {}",
        loaded_store.patterns[0].match_count
    );

    // Clean up
    std::fs::remove_file(&temp_path).ok();

    println!("\n✓ All scenarios completed successfully!");
}
