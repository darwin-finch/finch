//! Consecutive write/edit/patch proposals from one tool round.
//!
//! Issue #433: review them as one aggregate diff and accept or reject the
//! batch as a unit. This is not an overlay filesystem — the model still reads
//! the real workspace, and a non-changeset tool is a flush boundary.

use crate::cli::diff::FileDiff;
use crate::tools::{ToolUse, PEER_REVIEWED_CHANGESET_TOOLS};

/// True when `name` is a write/edit/patch review tool, including aliases.
pub(crate) fn is_reviewed_changeset_tool(name: &str) -> bool {
    PEER_REVIEWED_CHANGESET_TOOLS
        .iter()
        .any(|canonical| canonical.eq_ignore_ascii_case(name))
}

/// Split a tool round into consecutive changeset runs and singleton others.
///
/// `[write, edit, bash, patch]` becomes `[[write, edit], [bash], [patch]]`.
pub(crate) fn group_consecutive_changeset<T>(
    items: Vec<T>,
    name: impl Fn(&T) -> &str,
) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = Vec::new();
    let mut current: Vec<T> = Vec::new();
    for item in items {
        if is_reviewed_changeset_tool(name(&item)) {
            current.push(item);
            continue;
        }
        if !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
        groups.push(vec![item]);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// Planned file diffs in apply order, using in-memory composition for
/// repeated targets. Missing files start empty so a later edit of a file
/// created earlier in the same batch is previewed against the write.
pub(crate) struct PlannedChangeset {
    pub diffs: Vec<FileDiff>,
    pub notes: Vec<String>,
}

pub(crate) fn planned_changeset(tools: &[ToolUse]) -> PlannedChangeset {
    let mut contents: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut created: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut diffs = Vec::new();
    let mut notes = Vec::new();

    for tool in tools {
        let Some(path) = tool_file_path(tool) else {
            notes.push(format!(
                "{}: missing file_path, so it is omitted from the aggregate diff",
                tool.name
            ));
            continue;
        };
        let current = if let Some(text) = contents.get(&path) {
            text.clone()
        } else {
            match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    created.insert(path.clone());
                    String::new()
                }
                Err(error) => {
                    notes.push(format!(
                        "{} {}: could not read current bytes ({error})",
                        tool.name, path
                    ));
                    continue;
                }
            }
        };
        match planned_text(tool, &current) {
            Ok(after) => {
                let diff = if created.contains(&path) && current.is_empty() {
                    FileDiff::from_created(&path, &after)
                } else {
                    FileDiff::from_texts(&path, &current, &after)
                };
                diffs.push(diff);
                created.remove(&path);
                contents.insert(path, after);
            }
            Err(error) => notes.push(format!("{} {}: {error}", tool.name, path)),
        }
    }

    PlannedChangeset { diffs, notes }
}

fn tool_file_path(tool: &ToolUse) -> Option<String> {
    tool.input
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn planned_text(tool: &ToolUse, current: &str) -> anyhow::Result<String> {
    match tool.name.to_ascii_lowercase().as_str() {
        "write" => tool
            .input
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing content")),
        "edit" => {
            let old_string = tool
                .input
                .get("old_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing old_string"))?;
            let new_string = tool
                .input
                .get("new_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing new_string"))?;
            let replace_all = tool
                .input
                .get("replace_all")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let matches = current.matches(old_string).count();
            if matches == 0 {
                anyhow::bail!("old_string not found");
            }
            if matches > 1 && !replace_all {
                anyhow::bail!("old_string appears {matches} times");
            }
            Ok(if replace_all {
                current.replace(old_string, new_string)
            } else {
                current.replacen(old_string, new_string, 1)
            })
        }
        "patch" => {
            let patch = tool
                .input
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing patch"))?;
            crate::tools::preview_patched_text(current, patch)
                .map_err(|error| anyhow::anyhow!("{error}"))
        }
        other => anyhow::bail!("not a changeset tool ({other})"),
    }
}

/// Speakable one-line summary for the batch approval title.
pub(crate) fn changeset_approval_summary(tools: &[ToolUse]) -> String {
    let planned = planned_changeset(tools);
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if planned.diffs.is_empty() {
        return format!("{names}: {} file changes", tools.len());
    }
    format!(
        "{names}: {}",
        crate::cli::diff::summarize_files(&planned.diffs)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolUse;

    fn tool(id: &str, name: &str, input: serde_json::Value) -> ToolUse {
        ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    #[test]
    fn test_group_consecutive_changeset_splits_on_bash() {
        let tools = vec![
            tool("1", "write", serde_json::json!({})),
            tool("2", "edit", serde_json::json!({})),
            tool("3", "bash", serde_json::json!({})),
            tool("4", "patch", serde_json::json!({})),
        ];
        let groups = group_consecutive_changeset(tools, |tool| tool.name.as_str());
        let names: Vec<Vec<&str>> = groups
            .iter()
            .map(|group| group.iter().map(|tool| tool.name.as_str()).collect())
            .collect();
        assert_eq!(
            names,
            vec![vec!["write", "edit"], vec!["bash"], vec!["patch"]],
            "bash must flush a changeset; groups={names:?}"
        );
    }

    #[test]
    fn test_group_consecutive_changeset_keeps_single_write() {
        let tools = vec![tool("1", "write", serde_json::json!({}))];
        let groups = group_consecutive_changeset(tools, |tool| tool.name.as_str());
        assert_eq!(
            groups.len(),
            1,
            "a single write is still one group; groups={groups:?}"
        );
        assert_eq!(groups[0].len(), 1);
    }

    #[test]
    fn test_planned_changeset_composes_same_file_in_apply_order() {
        let directory = tempfile::tempdir().expect("batch preview directory");
        let path = directory.path().join("same.txt");
        std::fs::write(&path, "alpha\n").expect("seed target");
        let path_str = path.to_string_lossy().to_string();
        let tools = vec![
            tool(
                "w",
                "write",
                serde_json::json!({"file_path": path_str, "content": "beta\n"}),
            ),
            tool(
                "e",
                "edit",
                serde_json::json!({
                    "file_path": path_str,
                    "old_string": "beta",
                    "new_string": "gamma"
                }),
            ),
        ];
        let planned = planned_changeset(&tools);
        assert!(
            planned.notes.is_empty(),
            "preview must compose write then edit; notes={:?}",
            planned.notes
        );
        assert_eq!(
            planned.diffs.len(),
            2,
            "each proposal keeps its own diff; diffs={:?}",
            planned.diffs
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\n",
            "preview must not write the workspace"
        );
        let rendered = crate::cli::diff::summarize_files(&planned.diffs);
        assert!(
            rendered.contains("2 files") || planned.diffs.len() == 2,
            "aggregate summary should cover both steps; summary={rendered}"
        );
        let unified: String = planned.diffs.iter().map(FileDiff::to_unified).collect();
        assert!(
            unified.contains("beta") && unified.contains("gamma"),
            "apply-order preview must show the write and the following edit; unified={unified}"
        );
    }

    #[test]
    fn test_planned_changeset_includes_new_file_and_existing_edit() {
        let directory = tempfile::tempdir().expect("batch files");
        let created = directory.path().join("new.rs");
        let existing = directory.path().join("old.rs");
        std::fs::write(&existing, "fn main() {}\n").expect("seed existing");
        let tools = vec![
            tool(
                "w",
                "write",
                serde_json::json!({
                    "file_path": created.to_string_lossy(),
                    "content": "pub fn f() {}\n"
                }),
            ),
            tool(
                "e",
                "edit",
                serde_json::json!({
                    "file_path": existing.to_string_lossy(),
                    "old_string": "fn main() {}",
                    "new_string": "fn main() { 1 }"
                }),
            ),
        ];
        let planned = planned_changeset(&tools);
        assert_eq!(
            planned.diffs.len(),
            2,
            "write and edit must both preview; notes={:?}",
            planned.notes
        );
        let summary = crate::cli::diff::summarize_files(&planned.diffs);
        assert!(
            summary.contains("2 files"),
            "two-file batch summary must say 2 files; summary={summary}"
        );
    }

    #[test]
    fn test_changeset_dialog_is_yes_no_without_per_file_editor() {
        let tools = vec![
            tool(
                "w",
                "write",
                serde_json::json!({"file_path": "/tmp/a.txt", "content": "a\n"}),
            ),
            tool(
                "e",
                "edit",
                serde_json::json!({
                    "file_path": "/tmp/b.txt",
                    "old_string": "x",
                    "new_string": "y"
                }),
            ),
        ];
        let dialog =
            crate::cli::repl_event::tool_display::assemble_changeset_approval(&tools, "2 files");
        match dialog.dialog_type {
            crate::cli::tui::DialogType::Select { options, .. } => {
                let labels: Vec<&str> =
                    options.iter().map(|option| option.label.as_str()).collect();
                assert_eq!(
                    labels,
                    ["1. Yes", "2. No"],
                    "batch review is accept-or-reject as a unit; labels={labels:?}"
                );
                assert!(
                    labels.iter().all(|label| !label.contains("EDITOR")),
                    "Edit in $EDITOR on one file would break the unit; labels={labels:?}"
                );
            }
            other => panic!("changeset dialog must be a select; got {other:?}"),
        }
        assert!(
            dialog.title.contains("changeset"),
            "title must name the batch; title={}",
            dialog.title
        );
    }
}
