use std::process::Command;

fn run_finch(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(args)
        .output()
        .expect("typed Finch production boundary should launch")
}

#[test]
fn test_direct_forth_failure_renders_source_cited_structured_diagnostic() {
    let output = run_finch(&["--forth", "3 4 + say"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rendered = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    assert!(
        !output.status.success(),
        "invalid Co-Forth unexpectedly completed; {rendered}"
    );
    for required in [
        "E-TYPE-002 · verification error at direct-cli.forth:1:7",
        "3 4 + say",
        "      ^^^",
        "`say` expected string, but received int produced by `+` at direct-cli.forth:1:5",
        "Hint: convert it first: 3 4 + int-to-string say",
    ] {
        assert!(
            stderr.contains(required),
            "direct Co-Forth diagnostic omitted {required:?}; {rendered}"
        );
    }
    assert_eq!(
        stderr.matches("Hint:").count(),
        1,
        "direct Co-Forth failure should emit one actionable correction; {rendered}"
    );
}

#[test]
fn test_direct_lisp_failure_renders_equivalent_structured_diagnostic() {
    let output = run_finch(&["--lisp", "(say (+ 3 4))"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid Finch Lisp unexpectedly completed; stderr={stderr:?}"
    );
    for required in [
        "E-TYPE-002 · verification error at direct-cli.lisp:1:2",
        "(say (+ 3 4))",
        " ^^^",
        "`say` expected string, but received int produced by `+` at direct-cli.lisp:1:7",
        "int-to-string",
    ] {
        assert!(
            stderr.contains(required),
            "direct Lisp diagnostic omitted {required:?}; stderr={stderr:?}"
        );
    }
}

#[test]
fn test_exec_failure_renders_script_source_identity_and_diagnostic() {
    let script = tempfile::NamedTempFile::new().expect("script fixture should be created");
    std::fs::write(
        script.path(),
        "#!/usr/bin/env finch --exec --language=forth\n3 4 + say\n",
    )
    .expect("script fixture should be written");
    let path = script.path().to_string_lossy().into_owned();
    let output = run_finch(&["--exec", &path]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid Finch script unexpectedly completed; stderr={stderr:?}"
    );
    for required in [
        "E-TYPE-002 · verification error",
        &path,
        ":2:7",
        "3 4 + say",
        "      ^^^",
        "`say` expected string, but received int produced by `+`",
        "Hint: convert it first: 3 4 + int-to-string say",
    ] {
        assert!(
            stderr.contains(required),
            "--exec diagnostic omitted {required:?}; stderr={stderr:?}"
        );
    }
    assert_eq!(
        stderr.matches("Hint:").count(),
        1,
        "--exec failure should emit one actionable correction, not duplicate hints; stderr={stderr:?}"
    );
}

#[test]
fn test_json_failure_retains_structured_diagnostic_fields_and_span() {
    let output = run_finch(&["--forth", "3 4 + say", "--json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid JSON-mode Co-Forth unexpectedly completed; stdout={stdout:?}; stderr={stderr:?}"
    );
    let outcome: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!("JSON outcome was not machine-readable: {error}; stdout={stdout:?}")
        });
    let diagnostic = &outcome["vm_diagnostics"][0];
    assert_eq!(
        diagnostic["code"], "E-TYPE-002",
        "JSON diagnostic lost its stable code; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["phase"], "verification",
        "JSON diagnostic lost its phase; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["primary"]["word"], "say",
        "JSON diagnostic lost its failing form; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["primary"]["span"]["source_id"], "direct-cli.forth",
        "JSON diagnostic lost its source identity; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["primary"]["span"]["start_byte"], 6,
        "JSON diagnostic lost its exact byte span; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["expected_types"][0]["kind"], "string",
        "JSON diagnostic lost its expected type; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["found_types"][0]["kind"], "int",
        "JSON diagnostic lost its found type; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["found_value_origin"]["word"], "+",
        "JSON diagnostic lost the proven producer word; diagnostic={diagnostic:#?}"
    );
    assert_eq!(
        diagnostic["found_value_origin"]["span"]["start_byte"], 4,
        "JSON diagnostic lost the proven producer span; diagnostic={diagnostic:#?}"
    );
    assert!(
        diagnostic["hints"]
            .as_array()
            .is_some_and(|hints| hints.iter().any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("int-to-string")))),
        "JSON diagnostic lost its actionable correction; diagnostic={diagnostic:#?}"
    );
}
