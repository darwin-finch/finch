// IMCPD live contract tests
//
// These verify the structural contracts between Finch's prompts and real LLMs:
// - Critique prompts produce parseable Vec<CritiqueItem> JSON
// - Plan generation produces numbered steps
// - The critique JSON contract holds across all configured providers
//
// These are the tests that unit tests _cannot_ cover — they depend on the LLM
// actually following the instruction format.
//
// Run: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_imcpd

use finch::claude::Message;
use finch::planning::{CritiqueItem, IMCPD_METHODOLOGY};
use finch::providers::ProviderRequest;

use crate::{all_available_providers, live_tests_enabled, make_provider, resolve_api_key};

/// The IMCPD critique prompt must produce parseable `Vec<CritiqueItem>` from a real LLM.
///
/// This validates the JSON contract that the critique parser depends on. If the LLM
/// doesn't follow the schema, the IMCPD loop silently degrades to no critiques.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_imcpd_critique_parses_to_critique_items() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("claude").is_none() {
        eprintln!("skip: no API key for claude");
        return;
    }

    let provider = make_provider("claude").expect("claude provider");

    let sample_plan = "1. Add JWT middleware to src/server/routes.rs\n\
                        2. Update Config to include jwt_secret field\n\
                        3. cargo test";

    let system = finch::providers::with_alignment(Some(IMCPD_METHODOLOGY));
    let user = format!(
        "Active personas: Security, Regression, Completeness\n\n\
         Critique this plan:\n\n{sample_plan}\n\n\
         Return a JSON array of critique items only. \
         Use exactly the schema described in the methodology. \
         Do not wrap in markdown code fences. \
         If there are no issues, return []."
    );

    let req = ProviderRequest::new(vec![Message::user(user)])
        .with_system(system)
        .with_max_tokens(512);

    let resp = provider
        .send_message(&req)
        .await
        .expect("claude request failed");
    let text = resp.text();
    let trimmed = text.trim();

    let items: Vec<CritiqueItem> = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "Claude critique not parseable as Vec<CritiqueItem>: {e}\nGot: {:?}",
            &trimmed[..trimmed.len().min(300)]
        )
    });

    // Validate structural invariants on every item
    for item in &items {
        assert!(
            item.severity >= 1 && item.severity <= 10,
            "severity out of range [1,10]: got {}",
            item.severity
        );
        assert!(
            item.confidence >= 1 && item.confidence <= 10,
            "confidence out of range [1,10]: got {}",
            item.confidence
        );
        assert!(!item.persona.is_empty(), "persona should not be empty");
        assert!(!item.concern.is_empty(), "concern should not be empty");
    }
}

/// Plan generation must produce at least 2 numbered steps.
///
/// This validates that the plan generation prompt (with alignment) produces a
/// structured output that can be used as input to the critique step.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_imcpd_plan_generation_produces_numbered_steps() {
    if !live_tests_enabled() {
        return;
    }
    if resolve_api_key("claude").is_none() {
        eprintln!("skip: no API key for claude");
        return;
    }

    let provider = make_provider("claude").expect("claude provider");

    let req = ProviderRequest::new(vec![Message::user(format!(
        "{alignment}\n\n\
         You are an expert software engineer creating an implementation plan.\n\
         Task: Add a hello-world HTTP endpoint to an Axum server.\n\n\
         Generate a clear, numbered implementation plan. Requirements:\n\
         - Each step must be specific and actionable\n\
         - Name the exact files to modify or create\n\
         - Keep the scope tight — only what is necessary for this task\n\n\
         Return ONLY the numbered plan. No preamble. No post-amble.",
        alignment = finch::providers::UNIVERSAL_ALIGNMENT_PROMPT.trim(),
    ))])
    .with_max_tokens(512);

    let resp = provider
        .send_message(&req)
        .await
        .expect("claude request failed");
    let text = resp.text();

    let numbered_count = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.len() > 1
                && t.chars().next().map_or(false, |c| c.is_ascii_digit())
                && t.chars().nth(1).map_or(false, |c| c == '.' || c == ')')
        })
        .count();

    assert!(
        numbered_count >= 2,
        "Plan had fewer than 2 numbered steps ({numbered_count}):\n{text}"
    );
}

/// Cross-provider: the critique JSON contract holds for every configured provider.
///
/// All providers must return a valid JSON array (possibly empty) when given the
/// IMCPD critique prompt with alignment instructions active.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_imcpd_critique_parity_all_providers() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    let sample_plan = "1. Add auth middleware\n2. Update tests\n3. Commit";
    let system = finch::providers::with_alignment(Some(IMCPD_METHODOLOGY));

    for (name, provider) in providers {
        let user = format!(
            "Active personas: Security, Regression\n\n\
             Critique this plan:\n\n{sample_plan}\n\n\
             Return a JSON array of critique items only. \
             Use exactly the schema described in the methodology. \
             Do not wrap in markdown code fences. \
             If there are no issues, return []."
        );

        let req = ProviderRequest::new(vec![Message::user(user)])
            .with_system(system.clone())
            .with_max_tokens(512);

        let resp = provider
            .send_message(&req)
            .await
            .unwrap_or_else(|e| panic!("{name} request failed: {e}"));

        let text = resp.text();
        let trimmed = text.trim();
        let preview = &trimmed[..trimmed.len().min(120)];

        assert!(
            trimmed.starts_with('['),
            "{name} critique did not start with '[' (JSON contract broken)\nGot: {preview:?}"
        );

        serde_json::from_str::<serde_json::Value>(trimmed).unwrap_or_else(|e| {
            panic!("{name} critique is not valid JSON: {e}\nGot: {trimmed:?}")
        });
    }
}
