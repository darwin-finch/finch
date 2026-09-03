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

fn assert_requirement_excludes_affected(dependency: &str, requirement: &str) {
    let requirement = semver::VersionReq::parse(requirement).unwrap_or_else(|error| {
        panic!("Cap'n Proto dependency {dependency} has invalid version requirement '{requirement}': {error}")
    });
    for affected in ["0.20.6", "0.24.0-alpha.1"] {
        let affected = semver::Version::parse(affected).unwrap();
        assert!(
            !requirement.matches(&affected),
            "Cap'n Proto dependency {dependency} requirement '{requirement}' permits affected version {affected} under RUSTSEC-2025-0143; require stable 0.24.0 or newer"
        );
    }
}

fn assert_resolved_capnp_family_is_fixed(manifest_path: &Path) {
    let lock_path = manifest_path.with_file_name("Cargo.lock");
    let lock_text = std::fs::read_to_string(&lock_path).unwrap_or_else(|error| {
        panic!(
            "failed to read resolved dependency security contract from {}: {error}",
            lock_path.display()
        )
    });
    let lock: toml::Value = lock_text.parse().unwrap_or_else(|error| {
        panic!(
            "failed to parse resolved dependency security contract from {}: {error}",
            lock_path.display()
        )
    });
    let fixed = semver::Version::new(0, 24, 0);
    let mut found = std::collections::BTreeSet::new();
    for package in lock
        .get("package")
        .and_then(toml::Value::as_array)
        .expect("Cargo.lock must contain a package array for dependency security auditing")
    {
        let Some(name) = package.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        if !matches!(name, "capnp" | "capnp-rpc" | "capnpc") {
            continue;
        }
        let raw_version = package
            .get("version")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("Cargo.lock package {name} has no string version"));
        let version = semver::Version::parse(raw_version).unwrap_or_else(|error| {
            panic!("Cargo.lock package {name} has invalid version '{raw_version}': {error}")
        });
        assert!(
            version >= fixed,
            "Cargo.lock resolves {name} to vulnerable version {version} under RUSTSEC-2025-0143; resolve capnp, capnp-rpc, and capnpc to stable 0.24.0 or newer"
        );
        found.insert(name);
    }
    assert_eq!(
        found,
        std::collections::BTreeSet::from(["capnp", "capnp-rpc", "capnpc"]),
        "Cargo.lock must resolve the complete Cap'n Proto dependency family; found {found:?}"
    );
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

    assert_requirement_excludes_affected("capnp", capnp);
    assert_requirement_excludes_affected("capnp-rpc", capnp_rpc);
    assert_requirement_excludes_affected("capnpc", capnpc);
    assert_resolved_capnp_family_is_fixed(&manifest_path);
}
