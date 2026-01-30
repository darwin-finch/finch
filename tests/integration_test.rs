// Integration tests for Shammah

use shammah::crisis::CrisisDetector;
use shammah::router::{RouteDecision, Router};

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
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    let router = Router::new(crisis_detector);

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
fn test_router_no_match_forwarding() {
    let crisis_path = std::path::PathBuf::from("data/crisis_keywords.json");
    let crisis_detector =
        CrisisDetector::load_from_file(&crisis_path).expect("Failed to load crisis keywords");

    let router = Router::new(crisis_detector);

    // All non-crisis queries should be forwarded (patterns removed)
    let decision = router.route("How do I implement a binary search tree in Rust?");
    match decision {
        RouteDecision::Forward { reason } => {
            assert_eq!(reason.as_str(), "no_match");
        }
        _ => panic!("Expected forward decision for no match"),
    }

    // Another non-crisis query
    let decision = router.route("What is the golden rule?");
    match decision {
        RouteDecision::Forward { reason } => {
            assert_eq!(reason.as_str(), "no_match");
        }
        _ => panic!("Expected forward decision (patterns removed)"),
    }
}
