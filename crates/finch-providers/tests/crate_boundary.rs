//! Crate-independence and public-contract smoke tests.

#[test]
fn package_graph_does_not_include_finch() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata packages array");
    let pkg = packages
        .iter()
        .find(|package| package["name"] == "finch-providers")
        .expect("finch-providers package in metadata");
    let declared = pkg["dependencies"]
        .as_array()
        .expect("finch-providers dependencies")
        .iter()
        .filter_map(|dep| dep["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !declared.iter().any(|name| *name == "finch"),
        "finch-providers declared a finch dependency: {declared:?}"
    );
    if let Some(nodes) = metadata["resolve"]["nodes"].as_array() {
        let id = pkg["id"].as_str().expect("package id");
        let node = nodes
            .iter()
            .find(|candidate| candidate["id"].as_str() == Some(id))
            .expect("resolved finch-providers node");
        let resolved = node["deps"]
            .as_array()
            .expect("resolved deps")
            .iter()
            .filter_map(|dep| dep["name"].as_str())
            .collect::<Vec<_>>();
        assert!(
            !resolved.iter().any(|name| *name == "finch"),
            "resolved finch-providers graph includes finch: {resolved:?}"
        );
    }
}

#[test]
fn adapters_do_not_import_or_invoke_tool_executor() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let adapters = [
        "claude.rs",
        "openai.rs",
        "gemini.rs",
        "chatgpt_subscription.rs",
        "chatgpt_subscription/tests.rs",
    ];
    let forbidden = ["ToolExecutor", "execute_tool("];
    let mut hits = Vec::new();
    for adapter in adapters {
        let path = root.join(adapter);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for (line_number, line) in source.lines().enumerate() {
            if forbidden.iter().any(|token| line.contains(token)) {
                hits.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    line_number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "provider adapters parse and emit events; they must never own or invoke ToolExecutor: {hits:?}"
    );
}

#[test]
fn openai_and_claude_emit_native_tool_call_events() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for adapter in ["openai.rs", "claude.rs"] {
        let source = std::fs::read_to_string(root.join(adapter))
            .unwrap_or_else(|error| panic!("read {adapter}: {error}"));
        assert!(
            source.contains("StreamChunk::ToolCallDelta"),
            "{adapter} must emit ToolCallDelta from native fragments"
        );
        assert!(
            source.contains("StreamChunk::ToolCallComplete"),
            "{adapter} must emit ToolCallComplete after validated JSON"
        );
    }
}

#[test]
fn public_contract_exports_provider_and_oauth_facades() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<finch_providers::ProviderRequest>();
    assert_send_sync::<finch_providers::oauth::OAuthDialectDescriptor>();
}
