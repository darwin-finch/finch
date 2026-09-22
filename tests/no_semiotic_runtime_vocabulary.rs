//! Absence gate for the deleted legacy semiotic runtime and the abandoned
//! peer registry / scatter / gas experiment (issue #92).
//!
//! The typed VM is the one semantic runtime. The migration-only peer registry
//! (`src/registry`), its gas/credit/debit/settlement ledger, and the semiotic
//! interpreter that consumed them are deleted; the negative command-surface
//! guards elsewhere pin the experiment's `/`-commands out of the CLI. This
//! gate complements those guards at the source level: if any of the
//! experiment's distinctive symbols reappear in `src/` or `crates/` outside
//! the allowlist below, the build fails here with the offending line.
//!
//! Every allowlist entry names its reason for being retained. Adding an entry
//! without a live reason is a regression of this gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Distinctive vocabulary of the deleted semiotic runtime and peer/gas
/// experiment. Generic English words ("credit", "settle", "balance") are
/// deliberately excluded — live, unrelated subsystems use them (xAI API
/// credit error classification, dialog settle lifecycle).
const FORBIDDEN_MARKERS: &[&str] = &[
    "semiotic",
    "scatter",
    "gas-send",
    "gas_send",
    "join-registry",
    "join_registry",
    "credits_ms",
    "debits_ms",
    "LedgerEntry",
    "debt_threshold",
    "DebtThreshold",
    "registry-set",
    "registry_set",
];

/// Files allowed to contain a forbidden marker, with the retained reason.
/// Keyed by repo-relative path; the value explains why the hit is not a
/// revival of the deleted runtime.
const ALLOWLIST: &[(&str, &str)] = &[
    (
        "crates/finch-programs/src/forth_tokens.rs",
        "legacy Co-Forth lexer retained as the stored-definition identity \
         extractor (its only caller, forth_definition_identity, persists the \
         name of every Forth definition already on disk); the peer/scatter \
         word branches are inert — the tokens they emit have no consumer",
    ),
    (
        "src/main.rs",
        "the `scatter\\\"` string-literal opener in is_clearly_forth keeps \
         prose/program classification consistent with the retained lexer; \
         removing it would be a behavior change, not a deletion",
    ),
    (
        "src/cli/commands.rs",
        "negative guard tests asserting the experiment's commands stay out of \
         the command surface (removed_legacy_registry_commands_*)",
    ),
    (
        "src/generators/claude.rs",
        "negative guard test asserting the experiment's commands stay out of \
         the provider request reference",
    ),
    (
        "crates/finch-tui/src/command_autocomplete.rs",
        "negative guard test asserting the experiment's commands are never \
         suggested by autocomplete",
    ),
    (
        "tests/no_semiotic_runtime_vocabulary.rs",
        "this gate itself, which names the forbidden markers",
    ),
];

/// Rust source roots scanned by this gate.
const SOURCE_ROOTS: &[&str] = &["src", "crates"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn deleted_semiotic_runtime_symbols_stay_deleted() {
    let root = repo_root();
    let mut sources = Vec::new();
    for base in SOURCE_ROOTS {
        collect_rust_sources(&root.join(base), &mut sources);
    }
    assert!(
        sources.len() > 100,
        "absence gate found only {} Rust sources under {:?}; the scan itself is broken",
        sources.len(),
        SOURCE_ROOTS
    );

    let mut violations: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in &sources {
        let rel = source
            .strip_prefix(&root)
            .unwrap_or(source)
            .to_string_lossy()
            .replace('\\', "/");
        let body = fs::read_to_string(source)
            .unwrap_or_else(|error| panic!("absence gate failed to read {}: {error}", rel));
        let reasons: Vec<&(&str, &str)> =
            ALLOWLIST.iter().filter(|(path, _)| *path == rel).collect();
        for (line_no, line) in body.lines().enumerate() {
            let Some(marker) = FORBIDDEN_MARKERS
                .iter()
                .find(|marker| line.contains(**marker))
            else {
                continue;
            };
            if let Some((_, reason)) = reasons.first() {
                assert!(
                    !reason.is_empty(),
                    "allowlist entry for {rel} must name its retained reason"
                );
                continue;
            }
            violations.entry(rel.clone()).or_default().push(format!(
                "  {rel}:{line_no}: forbidden marker {marker:?} in: {}",
                line.trim()
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "the deleted legacy semiotic runtime / peer-gas experiment's symbols \
         reappeared outside the absence-gate allowlist. Each entry below needs \
         either deletion or an allowlist entry naming why it is deliberately \
         retained:\n{}",
        violations
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
