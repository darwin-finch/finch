// Cross-provider behavioral parity tests
//
// These tests iterate over ALL configured providers and assert that the same
// structural contract holds for each one. This is the key test suite for the
// "swap to cheapest provider" goal: if a provider fails here, it cannot be
// used as a drop-in replacement.
//
// Run: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_parity

use finch::claude::Message;
use finch::providers::ProviderRequest;

use crate::{all_available_providers, live_tests_enabled};

/// Every configured provider must return non-empty text for a simple prompt.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_nonempty_response() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    for (name, provider) in providers {
        let req = ProviderRequest::new(vec![Message::user("Say: ready")])
            .with_max_tokens(16);
        let resp = provider
            .send_message(&req)
            .await
            .unwrap_or_else(|e| panic!("{name} request failed: {e}"));
        assert!(
            !resp.text().trim().is_empty(),
            "{name} returned empty response"
        );
    }
}

/// Every provider must return a bare JSON array when the alignment prompt is active
/// and the user explicitly requests JSON output.
///
/// This validates the core "alignment prompt works" contract that lets us safely
/// swap providers in the IMPCPD critique loop.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_returns_bare_json_with_alignment() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    let system = finch::providers::with_alignment(None);

    for (name, provider) in providers {
        let req = ProviderRequest::new(vec![Message::user(
            "Return a JSON array of exactly 2 strings. Example: [\"a\",\"b\"]. \
             Return ONLY the JSON array, nothing else.",
        )])
        .with_system(system.clone())
        .with_max_tokens(64);

        let resp = provider
            .send_message(&req)
            .await
            .unwrap_or_else(|e| panic!("{name} request failed: {e}"));

        let text = resp.text();
        let trimmed = text.trim();
        let preview = &trimmed[..trimmed.len().min(120)];

        assert!(
            trimmed.starts_with('['),
            "{name} response did not start with '[' (alignment prompt not respected)\nGot: {preview:?}"
        );

        serde_json::from_str::<serde_json::Value>(trimmed).unwrap_or_else(|e| {
            panic!("{name} response was not valid JSON: {e}\nGot: {trimmed:?}")
        });
    }
}

/// Every provider must respect max_tokens (response length is bounded).
///
/// 50 tokens ≈ ~200 characters for most tokenizers. We allow 3× slack to
/// account for differences in tokenizer encoding across providers.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_respects_max_tokens() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    for (name, provider) in providers {
        let req = ProviderRequest::new(vec![Message::user(
            "Write a very long essay about everything in the universe.",
        )])
        .with_max_tokens(50);

        let resp = provider
            .send_message(&req)
            .await
            .unwrap_or_else(|e| panic!("{name} request failed: {e}"));

        // 50 tokens ≈ ~200 chars; 3× slack = 600 chars
        assert!(
            resp.text().len() < 600,
            "{name} response is suspiciously long ({} chars) for max_tokens=50",
            resp.text().len()
        );
    }
}
