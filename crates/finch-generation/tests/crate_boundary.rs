//! Crate-independence and public-contract smoke tests.

#[test]
fn package_graph_does_not_include_finch() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
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
        .find(|package| package["name"] == "finch-generation")
        .expect("finch-generation package in metadata");
    let declared = pkg["dependencies"]
        .as_array()
        .expect("finch-generation dependencies")
        .iter()
        .filter_map(|dep| dep["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !declared.iter().any(|name| *name == "finch"),
        "finch-generation declared a finch dependency: {declared:?}"
    );
    assert!(
        declared.iter().any(|name| *name == "finch-providers"),
        "finch-generation must consume finch-providers: {declared:?}"
    );
    if let Some(nodes) = metadata["resolve"]["nodes"].as_array() {
        let id = pkg["id"].as_str().expect("package id");
        let node = nodes
            .iter()
            .find(|candidate| candidate["id"].as_str() == Some(id))
            .expect("resolved finch-generation node");
        let resolved = node["deps"]
            .as_array()
            .expect("resolved deps")
            .iter()
            .filter_map(|dep| dep["name"].as_str())
            .collect::<Vec<_>>();
        assert!(
            !resolved.iter().any(|name| *name == "finch"),
            "resolved finch-generation graph includes finch: {resolved:?}"
        );
    }
}

#[test]
fn public_contract_exports_generation_and_tool_events() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<finch_generation::GenerationRequest>();
    assert_send_sync::<finch_generation::GenerationEvent>();
    assert_send_sync::<finch_generation::ToolCall>();
    assert_send_sync::<finch_generation::ToolResult>();
    assert_send_sync::<finch_generation::GenerationSupervisor>();
}

#[test]
fn crate_source_does_not_invoke_tool_executor() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    collect(&root, "ToolExecutor", &mut hits);
    assert!(
        hits.is_empty(),
        "generation crate must not own or invoke ToolExecutor; found: {hits:?}"
    );
}

fn collect(dir: &std::path::Path, needle: &str, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, needle, hits);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('"') {
                continue;
            }
            if line.contains(needle) {
                hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    }
}
