// Per-provider smoke tests
//
// One minimal live test per provider: resolves a key, sends a trivial prompt,
// asserts the response is non-empty. These are the simplest possible end-to-end
// checks — useful for confirming a new API key works.
//
// Run all: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_
// Run one: FINCH_LIVE_TESTS=1 ANTHROPIC_API_KEY=sk-ant-... cargo test -- --include-ignored live_claude_minimal

use finch::claude::Message;
use finch::providers::{LlmProvider, ProviderRequest};

use crate::{live_tests_enabled, make_provider, resolve_api_key};

// ── Claude ────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_claude_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("claude").is_none() {
        eprintln!("skip: no API key for claude");
        return;
    }
    let provider = make_provider("claude").expect("claude provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("claude request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "claude returned empty response"
    );
}

// ── OpenAI ────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_openai_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("openai").is_none() {
        eprintln!("skip: no API key for openai");
        return;
    }
    let provider = make_provider("openai").expect("openai provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("openai request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "openai returned empty response"
    );
}

// ── Grok ──────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_grok_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("grok").is_none() {
        eprintln!("skip: no API key for grok");
        return;
    }
    let provider = make_provider("grok").expect("grok provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("grok request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "grok returned empty response"
    );
}

// ── Gemini ────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_gemini_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("gemini").is_none() {
        eprintln!("skip: no API key for gemini");
        return;
    }
    let provider = make_provider("gemini").expect("gemini provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("gemini request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "gemini returned empty response"
    );
}

// ── Mistral ───────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_mistral_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("mistral").is_none() {
        eprintln!("skip: no API key for mistral");
        return;
    }
    let provider = make_provider("mistral").expect("mistral provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("mistral request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "mistral returned empty response"
    );
}

// ── Groq ──────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_groq_minimal_response() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("groq").is_none() {
        eprintln!("skip: no API key for groq");
        return;
    }
    let provider = make_provider("groq").expect("groq provider");
    let req = ProviderRequest::new(vec![Message::user("Say: ok")]).with_max_tokens(16);
    let resp = provider
        .send_message(&req)
        .await
        .expect("groq request failed");
    assert!(
        !resp.text().trim().is_empty(),
        "groq returned empty response"
    );
}
