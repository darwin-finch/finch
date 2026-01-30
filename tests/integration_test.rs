// Integration tests for Shammah

use shammah::config::Config;
use shammah::crisis::CrisisDetector;
use shammah::patterns::{PatternLibrary, PatternMatcher};
use shammah::router::{RouteDecision, Router};

#[test]
fn test_pattern_matching() {
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library =
        PatternLibrary::load_from_file(&patterns_path).expect("Failed to load patterns");

    let pattern_matcher = PatternMatcher::new(pattern_library, 0.2);

    // Test reciprocity pattern
    let result = pattern_matcher.find_match("What is the golden rule?");
    assert!(result.is_some());
    let (pattern, confidence) = result.unwrap();
    assert_eq!(pattern.id, "reciprocity");
    assert!(confidence >= 0.2);
}

#[test]
fn test_crisis_detection() {
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    // Should detect self-harm
    assert!(crisis_detector.detect_crisis("I'm thinking about suicide"));
    assert!(crisis_detector.detect_crisis("I want to kill myself"));

    // Should not detect normal queries
    assert!(!crisis_detector.detect_crisis("What is the meaning of life?"));
    assert!(!crisis_detector.detect_crisis("How do I learn Rust?"));
}

#[test]
fn test_router_crisis_forwarding() {
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library =
        PatternLibrary::load_from_file(&patterns_path).expect("Failed to load patterns");

    let pattern_matcher = PatternMatcher::new(pattern_library, 0.2);

    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    let router = Router::new(pattern_matcher, crisis_detector);

    // Crisis should be forwarded
    let decision = router.route("I'm thinking about suicide");
    match decision {
        RouteDecision::Forward { reason } => {
            assert_eq!(reason.as_str(), "crisis");
        }
        _ => panic!("Expected forward decision for crisis"),
    }
}

#[test]
fn test_router_pattern_matching() {
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library =
        PatternLibrary::load_from_file(&patterns_path).expect("Failed to load patterns");

    let pattern_matcher = PatternMatcher::new(pattern_library, 0.2);

    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    let router = Router::new(pattern_matcher, crisis_detector);

    // Golden rule should match reciprocity locally
    let decision = router.route("What is the golden rule?");
    match decision {
        RouteDecision::Local {
            pattern,
            confidence,
        } => {
            assert_eq!(pattern.id, "reciprocity");
            assert!(confidence >= 0.2);
        }
        _ => panic!("Expected local decision for pattern match"),
    }
}

#[test]
fn test_router_no_match_forwarding() {
    let patterns_path = std::path::PathBuf::from("data/patterns.json");
    let pattern_library =
        PatternLibrary::load_from_file(&patterns_path).expect("Failed to load patterns");

    let pattern_matcher = PatternMatcher::new(pattern_library, 0.2);

    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    let router = Router::new(pattern_matcher, crisis_detector);

    // Random technical question should be forwarded
    let decision = router.route("How do I implement a binary search tree in Rust?");
    match decision {
        RouteDecision::Forward { reason } => {
            assert_eq!(reason.as_str(), "no_match");
        }
        _ => panic!("Expected forward decision for no match"),
    }
}
