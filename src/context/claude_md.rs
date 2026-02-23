// Auto-loading of CLAUDE.md / FINCH.md / CONTEXT.md / README.md files into the system prompt
//
// Matches Claude Code behavior: load ~/.claude/CLAUDE.md (user-level) first,
// then walk upward from cwd to root collecting any CLAUDE.md, FINCH.md,
// CONTEXT.md, or README.md found, and concatenate them outermost-first so
// project-specific instructions win.
//
// FINCH.md is supported as an open, tool-agnostic alternative to CLAUDE.md.
// CONTEXT.md is a neutral name that works across any AI assistant.
// README.md is loaded last as general project overview context.
// When multiple files exist in the same directory, all are loaded in order.

use std::path::Path;
use tracing::{debug, info};

/// Filenames we look for, in the order they are loaded within a single directory.
const CONTEXT_FILENAMES: &[&str] = &["CLAUDE.md", "FINCH.md", "CONTEXT.md", "README.md"];

/// Collect all CLAUDE.md / FINCH.md / CONTEXT.md / README.md context visible from `cwd`.
///
/// Load order (lowest → highest priority):
/// 1. `~/.claude/CLAUDE.md` — user-level defaults (Claude Code convention)
/// 2. `~/.finch/FINCH.md`   — user-level defaults (Finch-specific)
/// 3. Each `CLAUDE.md` / `FINCH.md` / `CONTEXT.md` / `README.md` found walking
///    from root down to `cwd` (outermost first, in filename order within same dir)
///
/// Returns `None` if no files were found or all were empty.
pub fn collect_claude_md_context(cwd: &Path) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    // 1. User-level: ~/.claude/CLAUDE.md  (Claude Code convention)
    if let Some(home) = dirs::home_dir() {
        let user_claude_md = home.join(".claude").join("CLAUDE.md");
        if let Some(content) = read_non_empty(&user_claude_md) {
            info!("Loaded user CLAUDE.md: {}", user_claude_md.display());
            sections.push(content);
        }

        // 2. User-level: ~/.finch/FINCH.md  (Finch convention)
        let user_finch_md = home.join(".finch").join("FINCH.md");
        if let Some(content) = read_non_empty(&user_finch_md) {
            info!("Loaded user FINCH.md: {}", user_finch_md.display());
            sections.push(content);
        }
    }

    // 3. Walk upward from cwd to root, collecting paths.
    //    Build a list of (dir, filename) pairs sorted outermost-first.
    let ancestor_dirs: Vec<std::path::PathBuf> = {
        let mut dirs: Vec<_> = cwd.ancestors().map(|p| p.to_path_buf()).collect();
        dirs.reverse(); // root first, cwd last
        dirs
    };

    for dir in &ancestor_dirs {
        for &filename in CONTEXT_FILENAMES {
            let path = dir.join(filename);
            if let Some(content) = read_non_empty(&path) {
                info!("Loaded project {}: {}", filename, path.display());
                sections.push(content);
            }
        }
    }

    if sections.is_empty() {
        debug!("No context files found from {}", cwd.display());
        return None;
    }

    info!(
        "Loaded {} context file(s) into system prompt",
        sections.len()
    );

    Some(sections.join("\n\n---\n\n"))
}

/// Read a file and return its contents if non-empty, otherwise `None`.
fn read_non_empty(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        Ok(_) => None,
        Err(e) => {
            debug!("Failed to read {}: {}", path.display(), e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &std::path::Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn returns_none_when_no_context_files() {
        let tmp = TempDir::new().unwrap();
        // No context files in tmp or any ancestor (the user-level ones may
        // exist, but we can't control that in tests — just check no panic).
        let _ = collect_claude_md_context(tmp.path());
    }

    #[test]
    fn loads_claude_md_from_cwd() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "CLAUDE.md",
            "# Project Instructions\nDo the thing.",
        );

        let result = collect_claude_md_context(tmp.path());
        assert!(result.is_some());
        assert!(result.unwrap().contains("Do the thing."));
    }

    #[test]
    fn loads_finch_md_from_cwd() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "FINCH.md",
            "# Finch Instructions\nUse iterators.",
        );

        let result = collect_claude_md_context(tmp.path());
        assert!(result.is_some());
        assert!(result.unwrap().contains("Use iterators."));
    }

    #[test]
    fn loads_context_md_from_cwd() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "CONTEXT.md",
            "# Context\nPrefer functional style.",
        );

        let result = collect_claude_md_context(tmp.path());
        assert!(result.is_some());
        assert!(result.unwrap().contains("Prefer functional style."));
    }

    #[test]
    fn loads_readme_md_from_cwd() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "README.md", "# My Project\nDoes cool things.");

        let result = collect_claude_md_context(tmp.path());
        assert!(result.is_some());
        assert!(result.unwrap().contains("Does cool things."));
    }

    #[test]
    fn loads_all_names_in_same_directory() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "CLAUDE.md", "claude instructions");
        write(tmp.path(), "FINCH.md", "finch instructions");
        write(tmp.path(), "CONTEXT.md", "context instructions");
        write(tmp.path(), "README.md", "readme instructions");

        let result = collect_claude_md_context(tmp.path());
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("claude instructions"));
        assert!(text.contains("finch instructions"));
        assert!(text.contains("context instructions"));
        assert!(text.contains("readme instructions"));
        // Load order: CLAUDE.md → FINCH.md → CONTEXT.md → README.md
        let claude_pos = text.find("claude instructions").unwrap();
        let finch_pos = text.find("finch instructions").unwrap();
        let context_pos = text.find("context instructions").unwrap();
        let readme_pos = text.find("readme instructions").unwrap();
        assert!(
            claude_pos < finch_pos,
            "CLAUDE.md should appear before FINCH.md"
        );
        assert!(
            finch_pos < context_pos,
            "FINCH.md should appear before CONTEXT.md"
        );
        assert!(
            context_pos < readme_pos,
            "CONTEXT.md should appear before README.md"
        );
    }

    #[test]
    fn skips_empty_files() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "CLAUDE.md", "   \n   ");
        write(tmp.path(), "FINCH.md", "   \n   ");

        // All-whitespace files should be ignored.
        if let Some(text) = collect_claude_md_context(tmp.path()) {
            assert!(!text.trim().is_empty());
        }
    }

    #[test]
    fn joins_multiple_sections_with_separator() {
        let outer = TempDir::new().unwrap();
        let inner = outer.path().join("subdir");
        fs::create_dir_all(&inner).unwrap();

        write(outer.path(), "CLAUDE.md", "outer instructions");
        write(&inner, "FINCH.md", "inner instructions");

        let result = collect_claude_md_context(&inner);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("outer instructions"));
        assert!(text.contains("inner instructions"));
        // Outer comes before inner (outermost-first)
        let outer_pos = text.find("outer instructions").unwrap();
        let inner_pos = text.find("inner instructions").unwrap();
        assert!(outer_pos < inner_pos, "outer should appear before inner");
        assert!(text.contains("---"));
    }
}
