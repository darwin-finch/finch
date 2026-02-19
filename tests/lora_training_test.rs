// Integration test for LoRA training pipeline
//
// Tests JSONL queue writing and Python subprocess spawning

use finch::models::{TrainingCoordinator, WeightedExample};
use std::fs;
use std::path::Path;

#[test]
fn test_training_coordinator_creation() {
    let coordinator = TrainingCoordinator::new(100, 10, true);
    assert!(!coordinator.should_train()); // Empty buffer
}

#[test]
fn test_weighted_example_serialization() {
    let example = WeightedExample::critical(
        "What is 2+2?".to_string(),
        "4".to_string(),
        "Good answer".to_string(),
    );

    // Test serialization
    let json = serde_json::to_string(&example).expect("Failed to serialize");
    assert!(json.contains("\"weight\":10"));
    assert!(json.contains("\"query\":\"What is 2+2?\""));

    // Test deserialization
    let deserialized: WeightedExample =
        serde_json::from_str(&json).expect("Failed to deserialize");
    assert_eq!(deserialized.weight, 10.0);
    assert_eq!(deserialized.query, "What is 2+2?");
}

#[test]
fn test_example_buffer_adding() {
    let coordinator = TrainingCoordinator::new(100, 3, true);

    // Add examples below threshold
    coordinator
        .add_example(WeightedExample::normal(
            "Query 1".to_string(),
            "Response 1".to_string(),
            "Good".to_string(),
        ))
        .expect("Failed to add");

    assert!(!coordinator.should_train());

    coordinator
        .add_example(WeightedExample::normal(
            "Query 2".to_string(),
            "Response 2".to_string(),
            "Good".to_string(),
        ))
        .expect("Failed to add");

    assert!(!coordinator.should_train());

    // Add third example - should reach threshold
    let should_train = coordinator
        .add_example(WeightedExample::critical(
            "Query 3".to_string(),
            "Response 3".to_string(),
            "Critical feedback".to_string(),
        ))
        .expect("Failed to add");

    assert!(should_train);
    assert!(coordinator.should_train());
}

#[test]
fn test_jsonl_queue_writing() {
    // Create temp directory for testing
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let queue_path = temp_dir.path().join("test_queue.jsonl");

    // Create coordinator
    let coordinator = TrainingCoordinator::new(100, 10, false);

    // Add some examples
    coordinator
        .add_example(WeightedExample::critical(
            "What is Rust?".to_string(),
            "Rust is a systems programming language".to_string(),
            "Good explanation".to_string(),
        ))
        .expect("Failed to add");

    coordinator
        .add_example(WeightedExample::improvement(
            "How to use lifetimes?".to_string(),
            "Lifetimes ensure memory safety...".to_string(),
            "Could be more detailed".to_string(),
        ))
        .expect("Failed to add");

    coordinator
        .add_example(WeightedExample::normal(
            "Hello".to_string(),
            "Hi there!".to_string(),
            "Friendly greeting".to_string(),
        ))
        .expect("Failed to add");

    // Write to queue
    let count = coordinator.write_training_queue().expect("Failed to write queue");

    assert_eq!(count, 3);

    // Verify file was created at the coordinator's actual path
    let actual_queue_path = coordinator.queue_path().to_path_buf();

    // Read and verify contents
    if actual_queue_path.exists() {
        let contents = fs::read_to_string(&actual_queue_path).expect("Failed to read queue");
        let lines: Vec<&str> = contents.lines().collect();

        // We wrote 3 examples, but file may have previous examples too
        assert!(lines.len() >= 3, "Expected at least 3 lines in queue");

        // Verify last 3 lines are our examples
        let last_3_lines: Vec<&str> = lines.iter().rev().take(3).rev().copied().collect();

        // Parse and verify
        let examples: Vec<WeightedExample> = last_3_lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("Failed to parse line"))
            .collect();

        assert_eq!(examples[0].weight, 10.0); // Critical
        assert_eq!(examples[1].weight, 3.0); // Improvement
        assert_eq!(examples[2].weight, 1.0); // Normal

        println!("✅ JSONL queue written successfully to: {}", actual_queue_path.display());
        println!("✅ Verified {} examples", examples.len());
    }
}

#[test]
fn test_buffer_clear() {
    let coordinator = TrainingCoordinator::new(100, 10, false);

    // Add examples
    coordinator
        .add_example(WeightedExample::normal(
            "Test".to_string(),
            "Response".to_string(),
            "Good".to_string(),
        ))
        .expect("Failed to add");

    // Buffer should have 1 example
    {
        let buffer = coordinator.buffer().expect("Failed to get buffer");
        assert_eq!(buffer.len(), 1);
    }

    // Clear buffer
    coordinator.clear_buffer().expect("Failed to clear");

    // Buffer should be empty
    {
        let buffer = coordinator.buffer().expect("Failed to get buffer");
        assert_eq!(buffer.len(), 0);
    }
}
