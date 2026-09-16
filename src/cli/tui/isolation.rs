//! Production-boundary proof that the terminal renderer does not name Finch's
//! poset, tool, or runtime vocabularies except at the documented injection.

use std::fs;
use std::path::{Path, PathBuf};

fn tui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cli/tui")
}

fn production_source(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "tui isolation scan could not read {}: {error}",
            path.display()
        )
    });
    strip_cfg_test_modules(&text)
}

/// Drop `#[cfg(test)]` modules so test fixtures are not mistaken for production leaks.
fn strip_cfg_test_modules(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if let Some(rel) = text[index..].find("#[cfg(test)]") {
            let start = index + rel;
            out.push_str(&text[index..start]);
            let after_attr = start + "#[cfg(test)]".len();
            let rest = &text[after_attr..];
            let trimmed = rest.trim_start();
            // Skip further attributes, then `pub? mod name` `{` or `;`.
            let mut cursor = after_attr + (rest.len() - trimmed.len());
            loop {
                let leftover = text[cursor..].trim_start();
                let skipped = text[cursor..].len() - leftover.len();
                cursor += skipped;
                if leftover.starts_with("#[") {
                    if let Some(end) = leftover.find(']') {
                        cursor += end + 1;
                        continue;
                    }
                }
                break;
            }
            let leftover = text[cursor..].trim_start();
            let skipped = text[cursor..].len() - leftover.len();
            cursor += skipped;
            if let Some(mod_rel) = leftover.find("mod ") {
                cursor += mod_rel + 4;
                if let Some(brace_or_semi) = leftover[mod_rel + 4..].find(['{', ';']) {
                    let token = leftover[mod_rel + 4..]
                        .as_bytes()
                        .get(brace_or_semi)
                        .copied();
                    cursor += 4 + brace_or_semi + 1;
                    if token == Some(b'{') {
                        let mut depth = 1;
                        while cursor < bytes.len() && depth > 0 {
                            match bytes[cursor] {
                                b'{' => depth += 1,
                                b'}' => depth -= 1,
                                _ => {}
                            }
                            cursor += 1;
                        }
                    }
                    // Keep newlines so reported line numbers stay aligned with the file.
                    for ch in text[start..cursor].chars() {
                        out.push(if ch == '\n' { '\n' } else { ' ' });
                    }
                    index = cursor;
                    continue;
                }
            }
            out.push_str(&text[start..after_attr]);
            index = after_attr;
        } else {
            out.push_str(&text[index..]);
            break;
        }
    }
    out
}

fn production_hits(needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let dir = tui_dir();
    let entries = fs::read_dir(&dir).unwrap_or_else(|error| {
        panic!(
            "tui isolation scan could not read {}: {error}",
            dir.display()
        )
    });
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("tui isolation scan could not read dirent: {error}"))
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // Test-only files, including this scanner (its needles would match themselves).
        if matches!(name, "isolation.rs" | "vt_oracle.rs") {
            continue;
        }
        let source = production_source(&path);
        for (line_no, line) in source.lines().enumerate() {
            if line.contains(needle) {
                hits.push(format!("{name}:{}:{}", line_no + 1, line.trim()));
            }
        }
    }
    hits.sort();
    hits
}

#[test]
fn test_tui_production_does_not_name_finch_tools_or_runtime() {
    let tools = production_hits("crate::tools");
    assert!(
        tools.is_empty(),
        "tui production must not name crate::tools; a dialog shows a name and summary, \
         and ToolUse is converted at the caller: {tools:?}"
    );
    let runtime = production_hits("crate::runtime");
    assert!(
        runtime.is_empty(),
        "tui production must not name crate::runtime; spreadsheet cells are formatted by \
         tui::cell_format, not host-I/O: {runtime:?}"
    );
}

#[test]
fn test_tui_production_keeps_finch_poset_at_the_injection_boundary() {
    let hits = production_hits("crate::poset");
    let leaked: Vec<&String> = hits
        .iter()
        .filter(|hit| !hit.starts_with("mod.rs:"))
        .collect();
    assert!(
        leaked.is_empty(),
        "Finch Poset may appear in production tui only at the injection boundary in mod.rs \
         (set_poset / graph_view_from_poset). Other files must draw GraphView: {hits:?}"
    );
    assert!(
        !hits.is_empty(),
        "expected the documented Poset injection in tui/mod.rs; if set_poset no longer takes \
         Poset, update src/cli/tui/AGENTS.md and this test. hits={hits:?}"
    );
}
