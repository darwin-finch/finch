//! IMPCPD — Iterative Multi-Perspective Code Plan Debugging.
//!
//! This module drives Finch's `/plan` command: it generates a numbered
//! implementation plan, then runs multi-persona adversarial critique passes
//! until the plan converges or the user approves it.

mod loop_runner;
mod personas;
mod types;

pub use loop_runner::PlanLoop;
pub use personas::select_active_personas;
pub use types::{
    ConvergenceResult, CritiqueItem, ImpcpdConfig, PlanIteration, PlanResult, UserFeedback,
};

/// The IMPCPD runtime methodology spec, embedded at compile time.
///
/// This is sent verbatim to the LLM as the critique system context.
/// The spec defines the six critique personas, their activation rules,
/// the severity×confidence scoring model, and the expected JSON output format.
pub const IMPCPD_METHODOLOGY: &str = include_str!("impcpd_methodology.md");

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn planning_facade_keeps_child_modules_private() {
        let facade = include_str!("mod.rs");
        let published = facade
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//"))
            .filter(|line| line.starts_with("pub mod "))
            .collect::<Vec<_>>();
        assert!(
            published.is_empty(),
            "planning facade must keep child modules private; found: {published:?}"
        );
    }

    #[test]
    fn planning_callers_use_facade_not_child_modules() {
        let children = ["loop_runner", "personas", "types"];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let planning = root.join("src/planning");
        let mut hits = Vec::new();
        for tree in ["src", "tests"] {
            collect_planning_child_imports(
                &root.join(tree),
                &root,
                &planning,
                &children,
                &mut hits,
            );
        }
        assert!(
            hits.is_empty(),
            "callers outside src/planning must use crate::planning::Item, not child modules; found: {hits:?}"
        );
    }

    #[test]
    fn planning_facade_reexports_caller_types() {
        let _ = std::any::type_name::<PlanLoop>();
        let _ = std::any::type_name::<ImpcpdConfig>();
        let _ = std::any::type_name::<PlanResult>();
        let _ = std::any::type_name::<PlanIteration>();
        let _ = std::any::type_name::<CritiqueItem>();
        let _ = std::any::type_name::<ConvergenceResult>();
        let _ = std::any::type_name::<UserFeedback>();
        let _ = select_active_personas;
        let _ = IMPCPD_METHODOLOGY;
    }

    fn collect_planning_child_imports(
        dir: &Path,
        root: &Path,
        planning: &Path,
        children: &[&str],
        hits: &mut Vec<String>,
    ) {
        if dir.starts_with(planning) {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                hits.push(format!("failed to read {}: {error}", dir.display()));
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_planning_child_imports(&path, root, planning, children, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                hits.push(format!("failed to read {}", path.display()));
                continue;
            };
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for child in children {
                    for prefix in ["crate::planning::", "finch::planning::"] {
                        let needle = [prefix, child, "::"].concat();
                        if line.contains(&needle) {
                            let rel = path.strip_prefix(root).unwrap_or(&path);
                            hits.push(format!("{}:{}", rel.display(), index + 1));
                        }
                    }
                }
            }
        }
    }
}
