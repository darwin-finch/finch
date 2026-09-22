// Absence gate for the removed Teacher vocabulary (#692).
//
// The legacy local/cloud "teacher" concept was deleted: configuration is the
// unified `[[providers]]` list, routing is `RouteDecision::Forward` over
// providers, and the persona `data/personas/teacher.toml` is a voice, not a
// provider. This scan fails if teacher-vocabulary symbols reappear in Rust
// sources, mirroring the `ModelFamily` single-declaration scan pattern in
// `src/models/unified_loader.rs`.
//
// The allowlist is explicit and content-pinned. Every entry is one of:
// - the persona voice (registrations and help text for `data/personas/teacher.toml`);
// - the single private legacy `[[teachers]]` load-and-migrate shim in the
//   config loader (unmigrated files in the wild must keep loading);
// - serde aliases that keep payloads written before the renames deserializable;
// - an unrelated sample-contact job title in `src/samples.rs`;
// - the command-parsing test that pins the removed `/teacher` spelling as unknown;
// - two routing log strings in `src/cli/repl_event/query_processor.rs` owned by
//   another in-flight change — listed here so their removal stays visible.

use std::path::{Path, PathBuf};

/// `(file path suffix, substring that must appear on the allowed line)`.
const ALLOWED_LINES: &[(&str, &str)] = &[
    // Persona voice — a persona named "teacher" is intentional and stays.
    ("src/config/persona.rs", "personas/teacher.toml"),
    ("src/config/persona.rs", "\"teacher\""),
    ("src/cli/repl.rs", "(\"teacher\", \"Patient"),
    ("src/cli/commands.rs", "expert-coder, teacher"),
    // Sample data — a contact's job title, unrelated to providers.
    ("src/samples.rs", "\"Teacher\","),
    // The one private load-and-migrate path for unmigrated `[[teachers]]`
    // config files, plus the regression tests that pin its behavior. Nothing
    // writes or documents the format anymore.
    ("src/config/loader.rs", "LegacyTeacherEntry"),
    ("src/config/loader.rs", "legacy_teacher_entry_to_provider"),
    ("src/config/loader.rs", "[[teachers]]"),
    ("src/config/loader.rs", "legacy teachers/backend"),
    ("src/config/loader.rs", "toml_config.teachers"),
    ("src/config/loader.rs", ".teachers"),
    ("src/config/loader.rs", "test_legacy_teachers_config"),
    ("src/config/loader.rs", "every legacy teacher row"),
    ("src/config/loader.rs", "contains(\"teachers\")"),
    ("src/config/settings.rs", "[[teachers]]"),
    // Wire-compat aliases: old payloads keep deserializing after the rename.
    (
        "crates/finch-node/src/stats.rs",
        "alias = \"teacher_queries\"",
    ),
    (
        "crates/finch-node/src/lib.rs",
        "alias = \"has_teacher_api\"",
    ),
    // Pins the removed command spelling as unknown; owned by this change.
    ("src/cli/commands.rs", "Command::parse(\"/teacher"),
    // Log strings only; another in-flight change owns this file.
    (
        "src/cli/repl_event/query_processor.rs",
        "Client-side routing: teacher",
    ),
];

fn scan_for_teacher_vocabulary(dir: &Path, root: &Path, violations: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            scan_for_teacher_vocabulary(&path, root, violations);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let suffix = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        for (index, line) in source.lines().enumerate() {
            if !line.to_lowercase().contains("teacher") {
                continue;
            }
            let allowed = ALLOWED_LINES
                .iter()
                .any(|(file, marker)| suffix.ends_with(file) && line.contains(marker));
            if !allowed {
                violations.push(format!("{}:{}: {}", suffix, index + 1, line.trim()));
            }
        }
    }
}

#[test]
fn test_no_teacher_vocabulary_in_rust_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for tree in ["src", "crates"] {
        scan_for_teacher_vocabulary(&root.join(tree), &root, &mut violations);
    }
    assert!(
        violations.is_empty(),
        "the Teacher vocabulary was removed (#692): local/cloud providers on one \
         [[providers]] list replaced the legacy teacher config and naming. Teacher \
         vocabulary reappeared in Rust sources — remove it, or extend the explicit \
         allowlist in tests/no_teacher_vocabulary.rs only for the persona voice, the \
         legacy load-migrate shim, or wire-compat serde aliases. Violations:\n{}",
        violations.join("\n")
    );
}
