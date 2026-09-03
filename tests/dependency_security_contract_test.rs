//! Security contracts for dependency declarations that protect production boundaries.

use std::path::Path;

fn dependency_version<'a>(
    dependencies: &'a toml::Value,
    section: &str,
    dependency: &str,
) -> &'a str {
    let declaration = dependencies
        .get(dependency)
        .unwrap_or_else(|| {
            panic!(
                "Cargo.toml must declare {dependency} in [{section}] so its security version is auditable"
            )
        });

    declaration
        .as_str()
        .or_else(|| declaration.get("version").and_then(toml::Value::as_str))
        .unwrap_or_else(|| {
            panic!(
                "Cargo.toml [{section}] {dependency} must have an explicit version for security auditing; got {declaration}"
            )
        })
}

fn capnp_series(version: &str) -> (u64, u64) {
    let normalized = version.trim_start_matches(['=', '^', '~']);
    let mut components = normalized.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("Cap'n Proto version '{version}' has no numeric major version"));
    let minor = components
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("Cap'n Proto version '{version}' has no numeric minor version"));
    (major, minor)
}

#[test]
fn test_capnp_dependency_family_excludes_rustsec_2025_0143() {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|error| {
        panic!(
            "failed to read dependency security contract from {}: {error}",
            manifest_path.display()
        )
    });
    let manifest: toml::Value = manifest_text.parse().unwrap_or_else(|error| {
        panic!(
            "failed to parse dependency security contract from {}: {error}",
            manifest_path.display()
        )
    });

    let unix_dependencies = manifest
        .get("target")
        .and_then(|value| value.get("cfg(unix)"))
        .and_then(|value| value.get("dependencies"))
        .expect("Cargo.toml must contain [target.'cfg(unix)'.dependencies] for Unix IPC crates");
    let build_dependencies = manifest
        .get("build-dependencies")
        .expect("Cargo.toml must contain [build-dependencies] for the Cap'n Proto schema compiler");
    let capnp = dependency_version(
        unix_dependencies,
        "target.'cfg(unix)'.dependencies",
        "capnp",
    );
    let capnp_rpc = dependency_version(
        unix_dependencies,
        "target.'cfg(unix)'.dependencies",
        "capnp-rpc",
    );
    let capnpc = dependency_version(build_dependencies, "build-dependencies", "capnpc");
    assert_eq!(
        capnp, capnp_rpc,
        "Cap'n Proto runtime crates must remain on one compatible release series: capnp={capnp}, capnp-rpc={capnp_rpc}"
    );
    assert_eq!(
        capnp, capnpc,
        "Cap'n Proto runtime and schema compiler must remain on one compatible release series: capnp={capnp}, capnpc={capnpc}"
    );

    let (major, minor) = capnp_series(capnp);
    assert!(
        major > 0 || minor >= 24,
        "Cap'n Proto {capnp} is vulnerable to RUSTSEC-2025-0143; declare capnp, capnp-rpc, and capnpc at version 0.24 or newer"
    );
}
