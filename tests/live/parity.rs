// Cross-provider behavioral parity tests
//
// These tests iterate over ALL configured providers and assert that the same
// structural contract holds for each one. This is the key test suite for the
// "swap to cheapest provider" goal: if a provider fails here, it cannot be
// used as a drop-in replacement.
//
// Run: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_parity

use finch::claude::Message;
use finch::programs::{
    wire_repair_request, ExecutionEffect, ProgramLanguage, BOOT_CAPSULE, FORTH_LANGUAGE_DEFINITION,
    LISP_LANGUAGE_DEFINITION, VM_LANGUAGE_DEFINITION,
};
use finch::providers::ProviderRequest;
use finch::runtime::outcome::ExecutionStatus;
use finch::runtime::{ProgramRuntime, ProgramSubmission};
use std::time::Duration;

use crate::{all_available_providers, live_tests_enabled};

async fn execute_wire_source_in(runtime: &ProgramRuntime, source: &str) -> Result<String, String> {
    let language = ProgramLanguage::infer_wire_source(source).map_err(|error| error.to_string())?;
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

async fn execute_wire_source(source: &str) -> Result<String, String> {
    execute_wire_source_in(&ProgramRuntime::new(), source).await
}

fn source_only_wire_system() -> String {
    format!(
        "{}\n\n{}\n\nNo introspection tools are attached to this source-only conformance request. \
         The complete canonical language package follows; use it directly.\n\n{}\n\n{}\n\n{}",
        finch::generators::claude::CODING_SYSTEM_PROMPT,
        BOOT_CAPSULE,
        VM_LANGUAGE_DEFINITION,
        LISP_LANGUAGE_DEFINITION,
        FORTH_LANGUAGE_DEFINITION,
    )
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

    let system = source_only_wire_system();
    let cases = [
        ("Emit exactly `ready` to the user.", "ready"),
        (
            "Emit exactly two lines. The first line is `alpha`; the second line is the quoted word `\"beta\"`. Emit no other text.",
            "alpha\n\"beta\"",
        ),
        (
            "Compute 2 multiplied by 144 locally in the Finch VM and emit only the decimal result.",
            "288",
        ),
        (
            "In one Finch program, define a recursive factorial function, compute factorial 6, and emit only the decimal result.",
            "720",
        ),
        (
            "In one Finch program, create a closure that captures integer 10, apply it to integer 32 by adding the captured value, and emit only the decimal result.",
            "42",
        ),
        (
            "In one Finch program, create a producer fiber that yields integers 2 and 3 before returning integer 5, join it, and emit only its decimal terminal result.",
            "5",
        ),
        (
            "In one Finch program, use a typed while loop to increment an integer from 0 until it reaches 5, then emit only the decimal result.",
            "5",
        ),
        (
            "In one Finch program, construct a typed record with an integer field named `answer` equal to 42, project that field, and emit only the decimal result.",
            "42",
        ),
    ];

    for (name, provider) in providers {
        let mut first_pass = 0usize;
        let mut repaired = 0usize;
        let mut terminal = 0usize;
        for (request, expected) in cases {
            let initial = match tokio::time::timeout(
                Duration::from_secs(60),
                provider.send_message(
                    &ProviderRequest::new(vec![Message::user(request)])
                        .with_system(system.clone())
                        .with_max_tokens(512),
                ),
            )
            .await
            {
                Ok(Ok(response)) => response.text(),
                Ok(Err(error)) => {
                    terminal += 1;
                    eprintln!("{name}: wire request failed: {error}");
                    continue;
                }
                Err(_) => {
                    terminal += 1;
                    eprintln!("{name}: wire request exceeded 60 seconds");
                    continue;
                }
            };
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
                    let replacement = match tokio::time::timeout(
                        Duration::from_secs(60),
                        provider.send_message(
                            &ProviderRequest::new(vec![
                                Message::user(request),
                                Message::assistant(initial),
                                Message::user(repair),
                            ])
                            .with_system(system.clone())
                            .with_max_tokens(512),
                        ),
                    )
                    .await
                    {
                        Ok(Ok(response)) => response.text(),
                        Ok(Err(error)) => {
                            terminal += 1;
                            eprintln!("{name}: wire repair failed: {error}");
                            continue;
                        }
                        Err(_) => {
                            terminal += 1;
                            eprintln!("{name}: wire repair exceeded 60 seconds");
                            continue;
                        }
                    };
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

/// A provider conversation and its VM revision advance together: the second
/// turn must be able to call a typed word committed by the first turn. This is
/// the smallest fixture that distinguishes Finch from isolated code snippets.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_finch_wire_stateful_session() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    let system = source_only_wire_system();
    for (name, provider) in providers {
        let runtime = ProgramRuntime::new();
        let first_request = "Define a typed function named `triple` that multiplies an integer by 3, then emit exactly `registered`.";
        let first = tokio::time::timeout(
            Duration::from_secs(60),
            provider.send_message(
                &ProviderRequest::new(vec![Message::user(first_request)])
                    .with_system(system.clone())
                    .with_max_tokens(512),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: first stateful turn exceeded 60 seconds"))
        .unwrap_or_else(|error| panic!("{name}: first stateful turn failed: {error}"))
        .text();
        finch::programs::corpus::capture_with_runtime_from_env(
            &runtime,
            name,
            provider.default_model(),
            "live_conformance_stateful",
            finch::programs::corpus::WireCorpusAttempt::FirstPass,
            &first,
        );
        let first_output = execute_wire_source_in(&runtime, &first)
            .await
            .unwrap_or_else(|error| panic!("{name}: first stateful program failed: {error}"));
        assert_eq!(
            first_output, "registered",
            "{name}: unexpected first output"
        );

        let second_request =
            "Using the `triple` function already committed by your previous program, compute triple 14 and emit only the decimal result. Do not redefine `triple`.";
        let second = tokio::time::timeout(
            Duration::from_secs(60),
            provider.send_message(
                &ProviderRequest::new(vec![
                    Message::user(first_request),
                    Message::assistant(first),
                    Message::user(second_request),
                ])
                .with_system(system.clone())
                .with_max_tokens(512),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: second stateful turn exceeded 60 seconds"))
        .unwrap_or_else(|error| panic!("{name}: second stateful turn failed: {error}"))
        .text();
        finch::programs::corpus::capture_with_runtime_from_env(
            &runtime,
            name,
            provider.default_model(),
            "live_conformance_stateful",
            finch::programs::corpus::WireCorpusAttempt::FirstPass,
            &second,
        );
        let second_output = execute_wire_source_in(&runtime, &second)
            .await
            .unwrap_or_else(|error| panic!("{name}: second stateful program failed: {error}"));
        assert_eq!(second_output, "42", "{name}: unexpected second output");
    }
}

/// Feed every configured provider the same malformed prior submission and the
/// real structured repair request. This measures correction independently of
/// whether a provider happened to fail an ordinary first-pass fixture.
#[tokio::test]
#[ignore = "live — set FINCH_LIVE_TESTS=1"]
async fn live_parity_finch_wire_diagnostic_repair() {
    if !live_tests_enabled() {
        return;
    }
    let providers = all_available_providers();
    if providers.is_empty() {
        eprintln!("skip: no providers configured");
        return;
    }

    let system = source_only_wire_system();
    let request = "Compute 6 multiplied by 7 locally in Finch and emit only the decimal result.";
    let rejected = "The answer is 42.";
    let diagnostic = execute_wire_source_in(&ProgramRuntime::new(), rejected)
        .await
        .expect_err("raw prose must fail the wire contract");
    let repair = wire_repair_request(rejected, &diagnostic);

    for (name, provider) in providers {
        let runtime = ProgramRuntime::new();
        let replacement = tokio::time::timeout(
            Duration::from_secs(60),
            provider.send_message(
                &ProviderRequest::new(vec![
                    Message::user(request),
                    Message::assistant(rejected),
                    Message::user(repair.clone()),
                ])
                .with_system(system.clone())
                .with_max_tokens(512),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: diagnostic repair exceeded 60 seconds"))
        .unwrap_or_else(|error| panic!("{name}: diagnostic repair failed: {error}"))
        .text();
        finch::programs::corpus::capture_with_runtime_from_env(
            &runtime,
            name,
            provider.default_model(),
            "live_conformance_repair",
            finch::programs::corpus::WireCorpusAttempt::Repair,
            &replacement,
        );
        let output = execute_wire_source_in(&runtime, &replacement)
            .await
            .unwrap_or_else(|error| panic!("{name}: repaired program failed: {error}"));
        assert_eq!(output, "42", "{name}: unexpected repaired output");
    }
}
