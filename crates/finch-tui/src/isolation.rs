//! Production-boundary proof that the terminal renderer does not name Finch's
//! poset, tool, runtime, project-context, or AskUserQuestion wire vocabularies.

use std::fs;
use std::path::{Path, PathBuf};

fn tui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
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

/// Drop `#[cfg(test)]` *module* items so test fixtures are not mistaken for production leaks.
///
/// Only `mod name { ... }` and `mod name;` after the attribute are skipped. A `#[cfg(test)]`
/// on a fn, method, or `use` is left in place: treating the next `mod` in the file as that
/// attribute's item blanks the production between them — including a later live-area function
/// and any later production item after `TuiRenderer::new_headless`.
fn strip_cfg_test_modules(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while let Some(rel) = text[index..].find("#[cfg(test)]") {
        let start = index + rel;
        out.push_str(&text[index..start]);
        match test_module_end(text, start) {
            Some(end) => {
                for ch in text[start..end].chars() {
                    out.push(if ch == '\n' { '\n' } else { ' ' });
                }
                index = end;
            }
            None => {
                let attr_end = start + "#[cfg(test)]".len();
                out.push_str(&text[start..attr_end]);
                index = attr_end;
            }
        }
    }
    out.push_str(&text[index..]);
    out
}

fn skip_ws(text: &str, mut index: usize) -> usize {
    while index < text.len() {
        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn skip_ws_and_line_comments(text: &str, mut index: usize) -> usize {
    loop {
        index = skip_ws(text, index);
        if text[index..].starts_with("//") {
            match text[index..].find('\n') {
                Some(rel) => index += rel + 1,
                None => return text.len(),
            }
            continue;
        }
        return index;
    }
}

/// Byte offset just past a `#[cfg(test)]` module item starting at `attr_start`, if this
/// attribute actually annotates a `mod`.
fn test_module_end(text: &str, attr_start: usize) -> Option<usize> {
    if !text[attr_start..].starts_with("#[cfg(test)]") {
        return None;
    }
    let mut index = attr_start + "#[cfg(test)]".len();
    loop {
        index = skip_ws_and_line_comments(text, index);
        if text[index..].starts_with("#[") {
            let rel = text[index..].find(']')?;
            index += rel + 1;
            continue;
        }
        break;
    }
    index = skip_ws_and_line_comments(text, index);
    if text[index..].starts_with("pub(") {
        let rel = text[index..].find(')')?;
        index += rel + 1;
        index = skip_ws_and_line_comments(text, index);
    } else if text[index..].starts_with("pub") {
        let after = index + 3;
        if after < text.len() {
            let next = text.as_bytes()[after];
            if next.is_ascii_alphanumeric() || next == b'_' {
                return None;
            }
        }
        index = skip_ws_and_line_comments(text, after);
    }
    if !text[index..].starts_with("mod") {
        return None;
    }
    let after_mod = index + 3;
    if after_mod < text.len() {
        let next = text.as_bytes()[after_mod];
        if next.is_ascii_alphanumeric() || next == b'_' {
            return None;
        }
    }
    index = skip_ws_and_line_comments(text, after_mod);
    let ident_start = index;
    while index < text.len() {
        let next = text.as_bytes()[index];
        if next.is_ascii_alphanumeric() || next == b'_' {
            index += 1;
        } else {
            break;
        }
    }
    if index == ident_start {
        return None;
    }
    index = skip_ws_and_line_comments(text, index);
    let bytes = text.as_bytes();
    match bytes.get(index).copied() {
        Some(b';') => Some(index + 1),
        Some(b'{') => {
            let mut depth = 1usize;
            index += 1;
            while index < bytes.len() && depth > 0 {
                match bytes[index] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                index += 1;
            }
            Some(index)
        }
        _ => None,
    }
}

fn hits_in(source: &str, file: &str, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (line_no, line) in source.lines().enumerate() {
        if line.contains(needle) {
            hits.push(format!("{file}:{}:{}", line_no + 1, line.trim()));
        }
    }
    hits
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
        hits.extend(hits_in(&production_source(&path), name, needle));
    }
    hits.sort();
    hits
}

fn find_fn(source: &str, fn_name: &str) -> Option<usize> {
    let marker = format!("fn {fn_name}");
    let mut from = 0;
    while let Some(rel) = source[from..].find(&marker) {
        let start = from + rel;
        let after = start + marker.len();
        let next = source[after..].chars().next();
        if next.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_') {
            return Some(start);
        }
        from = after;
    }
    None
}

fn insert_into_fn(source: &str, fn_name: &str, payload: &str) -> String {
    let start = find_fn(source, fn_name).unwrap_or_else(|| {
        panic!("production scan dropped {fn_name}; a leak inside it would be invisible.\n{source}")
    });
    let rel_brace = source[start..].find('{').unwrap_or_else(|| {
        panic!("{fn_name} has no body in production scan; cannot prove a leak would be seen")
    });
    let at = start + rel_brace + 1;
    let mut poisoned = String::with_capacity(source.len() + payload.len());
    poisoned.push_str(&source[..at]);
    poisoned.push_str(payload);
    poisoned.push_str(&source[at..]);
    poisoned
}

#[test]
fn test_strip_does_not_treat_cfg_test_fn_or_use_as_a_module() {
    // Shape of src/lib.rs: a cfg(test) method, then production leak sites, then
    // `mod tests`. The old scanner took the next `mod ` after any `#[cfg(test)]` and blanked
    // everything between — including a production function and a later Poset leak.
    let src = concat!(
        "impl TuiRenderer {\n",
        "    #[cfg(test)]\n",
        "    pub(crate) fn new_headless() {}\n",
        "    pub(crate) fn production_after_test_fn() {\n",
        "        crate::runtime::example_call\n",
        "    }\n",
        "    pub fn production_poset_adapter() { crate::poset::Poset }\n",
        "}\n",
        "#[cfg(test)]\n",
        "use crate::tools::ToolUse;\n",
        "pub fn tool_approval() { crate::tools::ToolUse }\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    crate::runtime::only_in_tests\n",
        "    crate::tools::hidden\n",
        "}\n",
    );
    let stripped = strip_cfg_test_modules(src);
    let runtime = hits_in(&stripped, "fixture.rs", "crate::runtime");
    assert_eq!(
        runtime,
        ["fixture.rs:5:crate::runtime::example_call"],
        "#[cfg(test)] fn must not blank a later production function; \
         test-module names must stay hidden. hits={runtime:?}\n{stripped}"
    );
    assert!(
        stripped.contains("crate::poset::Poset"),
        "#[cfg(test)] fn blanked a later production Poset item: {stripped}"
    );
    let tools = hits_in(&stripped, "fixture.rs", "crate::tools");
    assert_eq!(
        tools,
        [
            "fixture.rs:10:use crate::tools::ToolUse;",
            "fixture.rs:11:pub fn tool_approval() { crate::tools::ToolUse }",
        ],
        "#[cfg(test)] use must not blank production tool_approval, and test-module \
         ToolUse must stay hidden. hits={tools:?}\n{stripped}"
    );
}

#[test]
fn test_scanner_would_fail_if_runtime_returned_to_draw_live_area() {
    let src = production_source(&tui_dir().join("lib.rs"));
    assert!(
        find_fn(&src, "draw_live_area").is_some(),
        "production scan dropped draw_live_area; a runtime leak in active renderer code \
         would be invisible to the isolation tests"
    );
    let poisoned = insert_into_fn(&src, "draw_live_area", " crate::runtime::example_call; ");
    let hits = hits_in(&poisoned, "lib.rs", "crate::runtime");
    assert!(
        hits.iter().any(|hit| hit.contains("example_call")),
        "scanner would not fail if crate::runtime were re-added inside production \
         draw_live_area: {hits:?}"
    );
}

#[test]
fn test_scanner_would_fail_if_tools_returned_to_tool_approval() {
    let src = production_source(&tui_dir().join("dialog.rs"));
    assert!(
        find_fn(&src, "tool_approval").is_some(),
        "production scan dropped Dialog::tool_approval; a crate::tools leak there would be invisible"
    );
    let poisoned = insert_into_fn(&src, "tool_approval", " crate::tools::ToolUse; ");
    let hits = hits_in(&poisoned, "dialog.rs", "crate::tools");
    assert!(
        hits.iter().any(|hit| hit.contains("ToolUse")),
        "scanner would not fail if crate::tools appeared inside production \
         Dialog::tool_approval: {hits:?}"
    );
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
        "tui production must not name crate::runtime; the renderer owns terminal projection, \
         not host effects or workbook parsing: {runtime:?}"
    );
}

#[test]
fn test_tui_production_does_not_name_finch_poset() {
    let hits = production_hits("crate::poset");
    assert!(
        hits.is_empty(),
        "tui production must not name Finch Poset; the active overlay paints only the injected \
         corner text: {hits:?}"
    );
}

#[test]
fn test_tui_production_does_not_name_project_context() {
    let hits = production_hits("crate::context");
    assert!(
        hits.is_empty(),
        "tui production must not own project filesystem mention policy; the CLI adapter injects \
         candidates and selection snapshots through MentionPort: {hits:?}"
    );
}

#[test]
fn test_tui_production_does_not_name_ask_user_question_wire_schema() {
    let module = production_hits("crate::cli::llm_dialogs");
    let input = production_hits("AskUserQuestionInput");
    let output = production_hits("AskUserQuestionOutput");
    assert!(
        module.is_empty() && input.is_empty() && output.is_empty(),
        "TUI must consume QuestionView instead of AskUserQuestion wire types; \
         module={module:?}, input={input:?}, output={output:?}"
    );
}

#[test]
fn test_tui_production_does_not_reach_up_for_owned_completion_state() {
    let autocomplete = production_hits("crate::cli::command_autocomplete");
    let suggestions = production_hits("SuggestionManager");
    assert!(
        autocomplete.is_empty() && suggestions.is_empty(),
        "TUI completion must remain local and the unused suggestion manager must not return; \
         autocomplete={autocomplete:?}, suggestions={suggestions:?}"
    );
}

#[test]
fn test_scanner_would_fail_if_context_returned_to_mention_completion() {
    let src = production_source(&tui_dir().join("lib.rs"));
    let poisoned = insert_into_fn(
        &src,
        "mention_query_from_textarea",
        " crate::context::mention::mention_query_at; ",
    );
    let hits = hits_in(&poisoned, "lib.rs", "crate::context");
    assert!(
        hits.iter().any(|hit| hit.contains("mention_query_at")),
        "scanner would not fail if project-context parsing returned to composer completion: \
         {hits:?}"
    );
}
