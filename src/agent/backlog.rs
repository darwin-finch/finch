// Agent task backlog — loaded from ~/.finch/tasks.toml

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Task execution status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// Task priority
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    High,
    Normal,
    Low,
}

/// A single task in the backlog
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    /// Unique task identifier (e.g. "001")
    pub id: String,

    /// Human-readable description of what to do
    pub description: String,

    /// Optional path to the git repository to work in
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    /// Current execution status
    pub status: TaskStatus,

    /// Execution priority
    #[serde(default = "default_priority")]
    pub priority: TaskPriority,

    /// Optional notes for the agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// Failure reason (set when status == Failed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Normal
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct BacklogFile {
    #[serde(default)]
    tasks: Vec<AgentTask>,
}

/// Manages the task backlog file
pub struct TaskBacklog {
    path: PathBuf,
    tasks: Vec<AgentTask>,
}

impl TaskBacklog {
    /// Load backlog from a TOML file (creates empty backlog if file doesn't exist)
    pub fn load(path: PathBuf) -> Result<Self> {
        let tasks = if path.exists() {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read task backlog from {}", path.display()))?;
            let file: BacklogFile =
                toml::from_str(&contents).context("Failed to parse tasks.toml")?;
            file.tasks
        } else {
            Vec::new()
        };
        Ok(Self { path, tasks })
    }

    /// Return the next pending task (high priority first, then normal, then low)
    pub fn next_pending(&self) -> Option<&AgentTask> {
        for priority in &[TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if let Some(task) = self
                .tasks
                .iter()
                .find(|t| t.status == TaskStatus::Pending && &t.priority == priority)
            {
                return Some(task);
            }
        }
        None
    }

    /// Mark a task as running and persist
    pub fn mark_running(&mut self, id: &str) -> Result<()> {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = TaskStatus::Running;
        }
        self.save()
    }

    /// Mark a task as done and persist
    pub fn mark_done(&mut self, id: &str) -> Result<()> {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = TaskStatus::Done;
            task.failure_reason = None;
        }
        self.save()
    }

    /// Mark a task as failed with a reason and persist
    pub fn mark_failed(&mut self, id: &str, reason: &str) -> Result<()> {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = TaskStatus::Failed;
            task.failure_reason = Some(reason.to_string());
        }
        self.save()
    }

    /// Reload from disk (picks up new tasks added while agent is running)
    pub fn reload(&mut self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let contents = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to reload task backlog from {}", self.path.display()))?;
        let file: BacklogFile =
            toml::from_str(&contents).context("Failed to parse tasks.toml on reload")?;

        // Merge: keep in-memory status for tasks we know about, add new ones
        let mut merged = file.tasks;
        for new_task in &mut merged {
            if let Some(existing) = self.tasks.iter().find(|t| t.id == new_task.id) {
                // Preserve in-memory status (e.g. don't reset "running" back to "pending")
                if existing.status != TaskStatus::Pending {
                    new_task.status = existing.status.clone();
                    new_task.failure_reason = existing.failure_reason.clone();
                }
            }
        }
        self.tasks = merged;
        Ok(())
    }

    /// All tasks (for inspection/logging)
    pub fn tasks(&self) -> &[AgentTask] {
        &self.tasks
    }

    fn save(&self) -> Result<()> {
        let file = BacklogFile {
            tasks: self.tasks.clone(),
        };
        let contents = toml::to_string_pretty(&file).context("Failed to serialize task backlog")?;
        fs::write(&self.path, &contents)
            .with_context(|| format!("Failed to write task backlog to {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn write_toml(content: &str) -> NamedTempFile {
        let f = NamedTempFile::new().unwrap();
        fs::write(f.path(), content).unwrap();
        f
    }

    const SINGLE_TASK: &str = r#"
[[tasks]]
id = "001"
description = "Test task"
status = "pending"
"#;

    // ── loading ────────────────────────────────────────────────────────────────

    #[test]
    fn test_load_nonexistent_file_gives_empty_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.toml");
        let backlog = TaskBacklog::load(path).unwrap();
        assert!(backlog.tasks().is_empty());
        assert!(backlog.next_pending().is_none());
    }

    #[test]
    fn test_load_empty_tasks_list() {
        let f = write_toml(""); // empty file
        let backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert!(backlog.tasks().is_empty());
    }

    #[test]
    fn test_load_preserves_notes_and_optional_fields() {
        let toml = r#"
[[tasks]]
id = "001"
description = "With extras"
status = "pending"
notes = "Focus on tests"
repo = "/some/repo"
"#;
        let f = write_toml(toml);
        let backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        let t = &backlog.tasks()[0];
        assert_eq!(t.notes.as_deref(), Some("Focus on tests"));
        assert_eq!(t.repo.as_deref(), Some("/some/repo"));
    }

    #[test]
    fn test_load_invalid_toml_returns_error() {
        let f = write_toml("[[tasks]\nnot valid toml {{{");
        let result = TaskBacklog::load(f.path().to_path_buf());
        assert!(result.is_err());
    }

    // ── priority ordering ─────────────────────────────────────────────────────

    #[test]
    fn test_priority_high_before_normal_before_low() {
        let toml = r#"
[[tasks]]
id = "low"
description = "Low"
status = "pending"
priority = "low"

[[tasks]]
id = "high"
description = "High"
status = "pending"
priority = "high"

[[tasks]]
id = "normal"
description = "Normal"
status = "pending"
priority = "normal"
"#;
        let f = write_toml(toml);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(backlog.next_pending().unwrap().id, "high");
        backlog.mark_done("high").unwrap();
        assert_eq!(backlog.next_pending().unwrap().id, "normal");
        backlog.mark_done("normal").unwrap();
        assert_eq!(backlog.next_pending().unwrap().id, "low");
        backlog.mark_done("low").unwrap();
        assert!(backlog.next_pending().is_none());
    }

    #[test]
    fn test_default_priority_is_normal() {
        // No priority field → should default to Normal
        let f = write_toml(SINGLE_TASK);
        let backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(backlog.tasks()[0].priority, TaskPriority::Normal);
    }

    #[test]
    fn test_skips_non_pending_tasks() {
        let toml = r#"
[[tasks]]
id = "done"
description = "Already done"
status = "done"

[[tasks]]
id = "failed"
description = "Already failed"
status = "failed"

[[tasks]]
id = "running"
description = "In flight"
status = "running"

[[tasks]]
id = "next"
description = "Actually pending"
status = "pending"
"#;
        let f = write_toml(toml);
        let backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        let next = backlog.next_pending().unwrap();
        assert_eq!(next.id, "next");
    }

    // ── state transitions ─────────────────────────────────────────────────────

    #[test]
    fn test_mark_running_persists_to_disk() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        backlog.mark_running("001").unwrap();

        // Reload fresh from disk
        let reloaded = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.tasks()[0].status, TaskStatus::Running);
        assert!(reloaded.next_pending().is_none()); // Running ≠ pending
    }

    #[test]
    fn test_mark_done_persists_to_disk() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        backlog.mark_done("001").unwrap();

        let reloaded = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.tasks()[0].status, TaskStatus::Done);
        assert!(reloaded.next_pending().is_none());
    }

    #[test]
    fn test_mark_done_clears_failure_reason() {
        let toml = r#"
[[tasks]]
id = "001"
description = "Test"
status = "failed"
failure_reason = "timeout"
"#;
        let f = write_toml(toml);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        backlog.mark_done("001").unwrap();
        assert!(backlog.tasks()[0].failure_reason.is_none());
    }

    #[test]
    fn test_mark_failed_sets_reason() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        backlog.mark_failed("001", "network timeout").unwrap();

        assert_eq!(backlog.tasks()[0].status, TaskStatus::Failed);
        assert_eq!(
            backlog.tasks()[0].failure_reason.as_deref(),
            Some("network timeout")
        );

        // Verify persisted
        let reloaded = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.tasks()[0].status, TaskStatus::Failed);
        assert_eq!(
            reloaded.tasks()[0].failure_reason.as_deref(),
            Some("network timeout")
        );
    }

    #[test]
    fn test_state_change_for_unknown_id_is_noop() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        // Should not error, just ignore
        backlog.mark_done("999").unwrap();
        assert_eq!(backlog.tasks()[0].status, TaskStatus::Pending);
    }

    // ── reload ────────────────────────────────────────────────────────────────

    #[test]
    fn test_reload_picks_up_new_tasks() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        assert_eq!(backlog.tasks().len(), 1);

        // Write a second task to disk while backlog is loaded
        let updated = r#"
[[tasks]]
id = "001"
description = "Test task"
status = "pending"

[[tasks]]
id = "002"
description = "New task"
status = "pending"
"#;
        fs::write(f.path(), updated).unwrap();
        backlog.reload().unwrap();
        assert_eq!(backlog.tasks().len(), 2);
    }

    #[test]
    fn test_reload_preserves_running_status() {
        let f = write_toml(SINGLE_TASK);
        let mut backlog = TaskBacklog::load(f.path().to_path_buf()).unwrap();
        backlog.mark_running("001").unwrap();

        // File on disk still shows "pending" (simulating external editor reset)
        fs::write(f.path(), SINGLE_TASK).unwrap();
        backlog.reload().unwrap();

        // In-memory "running" should be preserved (not reset to pending)
        assert_eq!(backlog.tasks()[0].status, TaskStatus::Running);
    }

    #[test]
    fn test_reload_on_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.toml");
        let mut backlog = TaskBacklog::load(path).unwrap();
        // File doesn't exist — reload should not error
        backlog.reload().unwrap();
        assert!(backlog.tasks().is_empty());
    }

    // ── serialisation round-trip ──────────────────────────────────────────────

    #[test]
    fn test_save_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.toml");

        // Load empty, save via mark_done on a task we inject manually
        let toml = r#"
[[tasks]]
id = "42"
description = "Round-trip task"
status = "pending"
priority = "high"
notes = "some notes"
repo = "/foo/bar"
"#;
        fs::write(&path, toml).unwrap();
        let mut backlog = TaskBacklog::load(path.clone()).unwrap();
        backlog.mark_done("42").unwrap();

        let fresh = TaskBacklog::load(path).unwrap();
        let t = &fresh.tasks()[0];
        assert_eq!(t.id, "42");
        assert_eq!(t.description, "Round-trip task");
        assert_eq!(t.status, TaskStatus::Done);
        assert_eq!(t.priority, TaskPriority::High);
        assert_eq!(t.notes.as_deref(), Some("some notes"));
        assert_eq!(t.repo.as_deref(), Some("/foo/bar"));
    }
}
