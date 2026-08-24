// Cross-provider behavioral parity tests
//
// These tests iterate over ALL configured providers and assert that the same
// structural contract holds for each one. This is the key test suite for the
// "swap to cheapest provider" goal: if a provider fails here, it cannot be
// used as a drop-in replacement.
//
// Run: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_parity

use finch::claude::Message;
use finch::programs::{wire_repair_request, ExecutionEffect, ProgramLanguage, BOOT_CAPSULE};
use finch::providers::ProviderRequest;
use finch::runtime::outcome::ExecutionStatus;
use finch::runtime::{ProgramRuntime, ProgramSubmission};

use crate::{all_available_providers, live_tests_enabled};

async fn execute_wire_source(source: &str) -> Result<String, String> {
    let language = ProgramLanguage::infer_wire_source(source).map_err(|error| error.to_string())?;
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit_typed_only(ProgramSubmission {
            language,
            source_id: Some(format!("live-conformance.{}", language.as_str())),
            source: source.to_string(),
            intent: "live provider wire conformance".into(),
            effect: ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .map_err(|error| error.to_string())?;
    if outcome.status == ExecutionStatus::Completed {
        Ok(outcome.output)
    } else {
        Err(outcome
            .vm_diagnostics
            .first()
            .map(ToString::to_string)
            .or_else(|| outcome.diagnostics.first().cloned())
            .unwrap_or_else(|| format!("wire execution ended as {:?}", outcome.status)))
    }
}

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
        let req = ProviderRequest::new(vec![Message::user("Say: ready")]).with_max_tokens(16);
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

/// Fixed provider-neutral Finch workload. Each response is parsed and executed
/// as source, not inspected for Forth/Lisp-looking text. One compile/link repair
/// turn mirrors the production wire receiver and the aggregate is printed as a
/// source-free conformance result.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_finch_wire_programs() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    let system = format!(
        "{}\n\n{}",
        finch::generators::claude::CODING_SYSTEM_PROMPT,
        BOOT_CAPSULE
    );
    let cases = [
        ("Emit exactly `ready` to the user.", "ready"),
        (
            "Compute 2 multiplied by 144 locally in the Finch VM and emit only the decimal result.",
            "288",
        ),
        (
            "In one Finch program, define a recursive factorial function, compute factorial 6, and emit only the decimal result.",
            "720",
        ),
    ];

    for (name, provider) in providers {
        let mut first_pass = 0usize;
        let mut repaired = 0usize;
        let mut terminal = 0usize;
        for (request, expected) in cases {
            let initial = provider
                .send_message(
                    &ProviderRequest::new(vec![Message::user(request)])
                        .with_system(system.clone())
                        .with_max_tokens(512),
                )
                .await
                .unwrap_or_else(|error| panic!("{name} wire request failed: {error}"))
                .text();
            finch::programs::corpus::capture_from_env(
                name,
                provider.default_model(),
                "live_conformance",
                finch::programs::corpus::WireCorpusAttempt::FirstPass,
                &initial,
            );
            match execute_wire_source(&initial).await {
                Ok(output) if output == expected => {
                    first_pass += 1;
                }
                Ok(output) => {
                    terminal += 1;
                    eprintln!(
                        "{name}: valid program produced unexpected output (expected {expected:?}, got {output:?})"
                    );
                }
                Err(diagnostic) => {
                    let repair = wire_repair_request(&initial, &diagnostic);
                    let replacement = provider
                        .send_message(
                            &ProviderRequest::new(vec![
                                Message::user(request),
                                Message::assistant(initial),
                                Message::user(repair),
                            ])
                            .with_system(system.clone())
                            .with_max_tokens(512),
                        )
                        .await
                        .unwrap_or_else(|error| panic!("{name} wire repair failed: {error}"))
                        .text();
                    finch::programs::corpus::capture_from_env(
                        name,
                        provider.default_model(),
                        "live_conformance",
                        finch::programs::corpus::WireCorpusAttempt::Repair,
                        &replacement,
                    );
                    match execute_wire_source(&replacement).await {
                        Ok(output) if output == expected => repaired += 1,
                        Ok(output) => {
                            terminal += 1;
                            eprintln!(
                                "{name}: repaired program produced unexpected output (expected {expected:?}, got {output:?})"
                            );
                        }
                        Err(error) => {
                            terminal += 1;
                            eprintln!("{name}: terminal wire diagnostic: {error}");
                        }
                    }
                }
            }
        }
        eprintln!(
            "{name}: sample={} first_pass={} repaired={} terminal={}",
            cases.len(),
            first_pass,
            repaired,
            terminal
        );
        assert_eq!(terminal, 0, "{name} failed the Finch wire workload");
    }
}
