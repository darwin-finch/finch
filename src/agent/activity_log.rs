// Activity logger — writes agent events to ~/.finch/agent_YYYY-MM-DD.jsonl

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// An event logged by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Agent picked up a task from the backlog
    TaskStart { id: String, desc: String },
    /// Agent invoked a tool
    ToolUse { tool: String, cmd: String },
    /// Agent committed changes to a git repo
    Commit {
        repo: String,
        hash: String,
        msg: String,
    },
    /// Task completed successfully
    TaskDone { id: String, duration_s: u64 },
    /// Task failed
    TaskFailed {
        id: String,
        duration_s: u64,
        reason: String,
    },
    /// Self-reflection / persona update
    Reflect { summary: String },
    /// Agent is sleeping waiting for new tasks
    Idle { sleep_s: u64 },
}

#[derive(Debug, Serialize)]
struct LogEntry<'a> {
    ts: String,
    #[serde(flatten)]
    event: &'a AgentEvent,
}

/// Writes agent activity to a daily JSONL log file
pub struct ActivityLogger {
    finch_dir: PathBuf,
}

impl ActivityLogger {
    /// Create a new logger (writes to ~/.finch/agent_YYYY-MM-DD.jsonl)
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let finch_dir = home.join(".finch");
        std::fs::create_dir_all(&finch_dir).context("Failed to create ~/.finch directory")?;
        Ok(Self { finch_dir })
    }

    /// Log an event
    pub fn log(&self, event: AgentEvent) -> Result<()> {
        let date = Local::now().format("%Y-%m-%d").to_string();
        let path = self.finch_dir.join(format!("agent_{}.jsonl", date));

        let ts = Utc::now().to_rfc3339();
        let entry = LogEntry { ts, event: &event };
        let json = serde_json::to_string(&entry).context("Failed to serialize activity event")?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open activity log: {}", path.display()))?;

        writeln!(file, "{}", json).context("Failed to write activity event")?;

        Ok(())
    }

    /// Return the path to today's log file
    pub fn today_path(&self) -> PathBuf {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.finch_dir.join(format!("agent_{}.jsonl", date))
    }

    /// Create a logger that writes to an arbitrary directory (for testing)
    #[cfg(test)]
    pub fn with_dir(finch_dir: PathBuf) -> Self {
        Self { finch_dir }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn logger_in_tempdir() -> (ActivityLogger, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let logger = ActivityLogger::with_dir(dir.path().to_path_buf());
        (logger, dir)
    }

    fn read_lines(logger: &ActivityLogger) -> Vec<serde_json::Value> {
        let path = logger.today_path();
        if !path.exists() {
            return Vec::new();
        }
        fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid JSON line"))
            .collect()
    }

    // ── basic I/O ─────────────────────────────────────────────────────────────

    #[test]
    fn test_log_creates_file() {
        let (logger, _dir) = logger_in_tempdir();
        assert!(!logger.today_path().exists());
        logger
            .log(AgentEvent::TaskStart {
                id: "001".into(),
                desc: "do something".into(),
            })
            .unwrap();
        assert!(logger.today_path().exists());
    }

    #[test]
    fn test_multiple_logs_append() {
        let (logger, _dir) = logger_in_tempdir();
        for i in 0..5 {
            logger.log(AgentEvent::Idle { sleep_s: i }).unwrap();
        }
        let lines = read_lines(&logger);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn test_each_line_is_valid_json() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::Reflect {
                summary: "learned a lot".into(),
            })
            .unwrap();
        let lines = read_lines(&logger);
        assert_eq!(lines.len(), 1);
        // Must have a "ts" field
        assert!(lines[0].get("ts").is_some());
    }

    #[test]
    fn test_log_has_timestamp() {
        let (logger, _dir) = logger_in_tempdir();
        logger.log(AgentEvent::Idle { sleep_s: 60 }).unwrap();
        let lines = read_lines(&logger);
        let ts = lines[0]["ts"].as_str().unwrap();
        // RFC 3339 timestamps contain a 'T'
        assert!(
            ts.contains('T'),
            "timestamp should be RFC 3339, got: {}",
            ts
        );
    }

    // ── event tag serialisation ───────────────────────────────────────────────

    #[test]
    fn test_task_start_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::TaskStart {
                id: "42".into(),
                desc: "refactor auth".into(),
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "task_start");
        assert_eq!(line["id"], "42");
        assert_eq!(line["desc"], "refactor auth");
    }

    #[test]
    fn test_tool_use_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::ToolUse {
                tool: "bash".into(),
                cmd: "cargo test".into(),
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "tool_use");
        assert_eq!(line["tool"], "bash");
        assert_eq!(line["cmd"], "cargo test");
    }

    #[test]
    fn test_commit_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::Commit {
                repo: "/projects/myapp".into(),
                hash: "abc1234".into(),
                msg: "feat: add tests".into(),
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "commit");
        assert_eq!(line["repo"], "/projects/myapp");
        assert_eq!(line["hash"], "abc1234");
        assert_eq!(line["msg"], "feat: add tests");
    }

    #[test]
    fn test_task_done_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::TaskDone {
                id: "007".into(),
                duration_s: 142,
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "task_done");
        assert_eq!(line["id"], "007");
        assert_eq!(line["duration_s"], 142);
    }

    #[test]
    fn test_task_failed_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::TaskFailed {
                id: "003".into(),
                duration_s: 30,
                reason: "build error".into(),
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "task_failed");
        assert_eq!(line["id"], "003");
        assert_eq!(line["duration_s"], 30);
        assert_eq!(line["reason"], "build error");
    }

    #[test]
    fn test_reflect_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger
            .log(AgentEvent::Reflect {
                summary: "Updated expertise: added TDD".into(),
            })
            .unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "reflect");
        assert_eq!(line["summary"], "Updated expertise: added TDD");
    }

    #[test]
    fn test_idle_event_tag() {
        let (logger, _dir) = logger_in_tempdir();
        logger.log(AgentEvent::Idle { sleep_s: 60 }).unwrap();
        let line = &read_lines(&logger)[0];
        assert_eq!(line["event"], "idle");
        assert_eq!(line["sleep_s"], 60);
    }

    // ── today_path format ─────────────────────────────────────────────────────

    #[test]
    fn test_today_path_includes_date() {
        let (logger, dir) = logger_in_tempdir();
        let path = logger.today_path();
        let filename = path.file_name().unwrap().to_str().unwrap();
        // e.g. "agent_2026-02-19.jsonl"
        assert!(filename.starts_with("agent_"), "got: {}", filename);
        assert!(filename.ends_with(".jsonl"), "got: {}", filename);
        let date_part = &filename["agent_".len()..filename.len() - ".jsonl".len()];
        // Should be YYYY-MM-DD
        assert_eq!(date_part.len(), 10, "date part: {}", date_part);
        assert!(date_part.contains('-'));
        // Must be inside our temp dir
        assert_eq!(path.parent().unwrap(), dir.path());
    }
}
