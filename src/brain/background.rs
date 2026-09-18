//! In-memory background-process tasks owned by the session process.
//!
//! Issue #754 execution slice: a [`BackgroundTaskManager`] holds long-lived
//! commands as child processes of whichever process runs the tool loop. Tasks
//! survive turn end — the start call returns a stable task ID immediately and
//! reader tasks keep draining output on the runtime, detached from the turn
//! future — and die only when stopped, reaped under bound pressure, or the
//! owning process goes away.
//!
//! Restart semantics are **kill, not adopt**: records are in-memory, so a
//! daemon restart loses running tasks by construction. Durable restart-adoptable
//! records are #88/#90 wire-format territory and deliberately out of scope.
//!
//! Process-group ownership stays with the supervisor: no job-control escape
//! API is used, so children remain in the owning
//! process's group and stop signals only the recorded direct child. Grandchild
//! processes of a `bash -c` invocation are not reaped by stop — a recorded
//! limitation, not an oversight.
//!
//! Locking: every mutex here is a [`std::sync::Mutex`] and no lock is held
//! across an `.await`, so the manager scan in [`Self::start`] can lock task
//! entries deterministically. Lock order is always `manager state → task
//! entry`; the watcher and reader tasks touch only the entry.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use uuid::Uuid;

/// Default bound on concurrently running background tasks. Starting beyond
/// this bound fails closed; a running process is never evicted to make room.
pub const DEFAULT_MAX_RUNNING_TASKS: usize = 16;

/// Default bound on total retained task entries (running + finished). Finished
/// entries are reaped oldest-first to make room; a start is rejected only when
/// every slot is held by a running task.
pub const DEFAULT_MAX_TOTAL_TASKS: usize = 64;

/// Default per-stream ring-buffer retention budget in bytes. Output beyond the
/// budget evicts the oldest lines; the count of discarded lines is reported.
pub const DEFAULT_RING_BYTES_PER_STREAM: usize = 64 * 1024;

/// Watcher poll interval while a task is running. Coarse liveness only; no
/// correctness assertion depends on this granularity.
const WATCHER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Stable identifier for one background task, returned immediately by
/// [`BackgroundTaskManager::start`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackgroundTaskId(String);

impl BackgroundTaskId {
    fn generate() -> Self {
        Self(format!("bg_{}", Uuid::new_v4().simple()))
    }

    /// The identifier as presented in tool results.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BackgroundTaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle state of one background task.
///
/// A task is `Running` until it exits naturally (`Completed`) or is stopped
/// (`Stopped`). Exactly one transition out of `Running` is ever recorded: the
/// writer that first observes terminal state wins, and later writers keep the
/// recorded terminal state untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum BackgroundTaskState {
    Running,
    /// Exited on its own; carries the raw exit code (`None` if signalled).
    Completed(Option<i32>),
    /// Stopped via [`BackgroundTaskManager::stop`] or process shutdown;
    /// carries the exit code or the terminating signal number.
    Stopped(ExitOutcome),
}

/// How a stopped process ended, recorded by the reaping stop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExitOutcome {
    /// Exited with a code (raced a natural completion).
    Code(i32),
    /// Killed by a signal; carries the signal number.
    Signal(i32),
}

impl std::fmt::Display for ExitOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit {code}"),
            Self::Signal(sig) => write!(f, "terminated by signal {sig}"),
        }
    }
}

impl BackgroundTaskState {
    /// True while the task still holds a slot a new task cannot take.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Byte-bounded line ring for one output stream.
///
/// Retention keeps the newest lines within the byte budget: pushing a line
/// evicts oldest lines until the retained byte count fits. A single line
/// larger than the budget is retained alone (bounded by the longest emitted
/// line), so total memory stays bounded by `budget + longest emitted line`.
#[derive(Debug)]
struct OutputRing {
    lines: VecDeque<String>,
    bytes: usize,
    budget: usize,
    discarded: u64,
}

impl OutputRing {
    fn new(budget: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            budget,
            discarded: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.lines.push_back(line);
        self.bytes += self.lines.back().map_or(0, |l| l.len()) + 1;
        while self.bytes > self.budget && self.lines.len() > 1 {
            if let Some(evicted) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(evicted.len() + 1);
                self.discarded += 1;
            }
        }
    }

    /// Joined retained lines plus the count of discarded earlier lines.
    fn snapshot(&self) -> (String, u64) {
        let mut text = String::with_capacity(self.bytes);
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            text.push_str(line);
        }
        (text, self.discarded)
    }
}

/// One background task's live record: process handle, rings, and state.
struct TaskEntry {
    id: BackgroundTaskId,
    command: String,
    description: String,
    pid: Option<u32>,
    started_at_unix_ms: u64,
    state: BackgroundTaskState,
    child: Option<tokio::process::Child>,
    stdout_ring: OutputRing,
    stderr_ring: OutputRing,
}

impl TaskEntry {
    fn snapshot(&self) -> BackgroundTaskSnapshot {
        let (stdout, stdout_discarded) = self.stdout_ring.snapshot();
        let (stderr, stderr_discarded) = self.stderr_ring.snapshot();
        BackgroundTaskSnapshot {
            id: self.id.clone(),
            command: self.command.clone(),
            description: self.description.clone(),
            pid: self.pid,
            started_at_unix_ms: self.started_at_unix_ms,
            state: self.state.clone(),
            stdout,
            stderr,
            stdout_discarded_lines: stdout_discarded,
            stderr_discarded_lines: stderr_discarded,
        }
    }
}

/// One poll result: identity, lifecycle state, and the bounded output rings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTaskSnapshot {
    pub id: BackgroundTaskId,
    pub command: String,
    pub description: String,
    pub pid: Option<u32>,
    pub started_at_unix_ms: u64,
    pub state: BackgroundTaskState,
    pub stdout: String,
    pub stderr: String,
    /// Lines evicted from the stdout ring by the retention budget.
    pub stdout_discarded_lines: u64,
    /// Lines evicted from the stderr ring by the retention budget.
    pub stderr_discarded_lines: u64,
}

impl BackgroundTaskSnapshot {
    /// Human-readable poll rendering used by tool results.
    pub fn render(&self) -> String {
        let mut text = match &self.state {
            BackgroundTaskState::Running => format!("Task {}: Running", self.id),
            BackgroundTaskState::Completed(Some(code)) => {
                format!("Task {}: Completed (exit {code})", self.id)
            }
            BackgroundTaskState::Completed(None) => {
                format!("Task {}: Completed (exited by signal)", self.id)
            }
            BackgroundTaskState::Stopped(outcome) => {
                format!("Task {}: Stopped ({outcome})", self.id)
            }
        };
        if !self.description.is_empty() {
            text.push_str(&format!("\nDescription: {}", self.description));
        }
        if let Some(pid) = self.pid {
            text.push_str(&format!("\nPID: {pid}"));
        }
        text.push_str(&format!(
            "\nStarted: {} ms since epoch\nCommand: {}",
            self.started_at_unix_ms, self.command
        ));
        for (label, ring, discarded) in [
            ("stdout", &self.stdout, self.stdout_discarded_lines),
            ("stderr", &self.stderr, self.stderr_discarded_lines),
        ] {
            text.push_str(&format!("\n--- {label}"));
            if discarded > 0 {
                text.push_str(&format!(
                    " ({discarded} earlier line{} discarded by the retention budget)",
                    if discarded == 1 { "" } else { "s" }
                ));
            }
            text.push_str(" ---\n");
            if ring.is_empty() {
                text.push_str("(no output retained)\n");
            } else {
                text.push_str(ring);
                if !ring.ends_with('\n') {
                    text.push('\n');
                }
            }
        }
        text
    }
}

struct ManagerState {
    tasks: HashMap<BackgroundTaskId, Arc<Mutex<TaskEntry>>>,
    order: Vec<BackgroundTaskId>,
}

/// Bounded lifecycle owner for long-lived commands.
///
/// Cloned handles share one table. The manager is dropped when the session
/// process tears down, which best-effort SIGKILLs every running child
/// (`kill_on_drop` on each parked handle); [`Self::shutdown_all`] is the
/// awaited, reaping variant for graceful shutdown.
pub struct BackgroundTaskManager {
    state: Mutex<ManagerState>,
    max_running: usize,
    max_total: usize,
    ring_bytes: usize,
}

impl Default for BackgroundTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundTaskManager {
    /// Manager with the documented default bounds.
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_RUNNING_TASKS,
            DEFAULT_MAX_TOTAL_TASKS,
            DEFAULT_RING_BYTES_PER_STREAM,
        )
    }

    /// Manager with explicit bounds (used by tests to make bounds reachable).
    pub fn with_limits(max_running: usize, max_total: usize, ring_bytes: usize) -> Self {
        Self {
            state: Mutex::new(ManagerState {
                tasks: HashMap::new(),
                order: Vec::new(),
            }),
            max_running,
            max_total,
            ring_bytes,
        }
    }

    /// Number of tasks currently in `Running` state.
    pub async fn running_count(&self) -> usize {
        let state = self.state.lock().expect("background manager state lock");
        state
            .tasks
            .values()
            .filter(|entry| {
                entry
                    .lock()
                    .expect("background task entry lock")
                    .state
                    .is_running()
            })
            .count()
    }

    /// Total retained entries, running and finished.
    pub async fn total_count(&self) -> usize {
        self.state
            .lock()
            .expect("background manager state lock")
            .tasks
            .len()
    }

    /// Start `command` under `bash -c` and return its task ID immediately.
    ///
    /// Output is drained by runtime-owned reader tasks into the per-stream
    /// rings, so the call does not block on the command and the pipes never
    /// fill. The child stays in this process's group; no job-control escape.
    pub async fn start(&self, command: &str, description: &str) -> Result<BackgroundTaskId> {
        let mut state = self.state.lock().expect("background manager state lock");

        let running = state
            .tasks
            .values()
            .filter(|entry| {
                entry
                    .lock()
                    .expect("background task entry lock")
                    .state
                    .is_running()
            })
            .count();
        anyhow::ensure!(
            running < self.max_running,
            "background task limit reached: {running} of {} running tasks; \
             stop one before starting another",
            self.max_running
        );
        while state.tasks.len() >= self.max_total {
            // Reap the oldest finished entry; reject only if none is finished.
            let victim = state
                .order
                .iter()
                .find(|id| {
                    state.tasks.get(*id).is_some_and(|entry| {
                        !entry
                            .lock()
                            .expect("background task entry lock")
                            .state
                            .is_running()
                    })
                })
                .cloned();
            let Some(victim) = victim else {
                anyhow::bail!(
                    "background task limit reached: all {} retained tasks are \
                     still running; stop one before starting another",
                    self.max_total
                );
            };
            state.tasks.remove(&victim);
            state.order.retain(|id| id != &victim);
        }

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Bounded-lifecycle safety net: if the manager drops without
            // shutdown, the child is killed instead of orphaned silently.
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Failed to spawn background command: {command}"))?;
        let pid = child.id();
        // Both streams are piped (above) and must be detached while the child
        // is still local, so readers never wait on a watcher or a stop that
        // holds the entry lock.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("background task stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("background task stderr was not piped"))?;

        let id = BackgroundTaskId::generate();
        let entry = Arc::new(Mutex::new(TaskEntry {
            id: id.clone(),
            command: command.to_string(),
            description: description.to_string(),
            pid,
            started_at_unix_ms: unix_millis_now(),
            state: BackgroundTaskState::Running,
            stdout_ring: OutputRing::new(self.ring_bytes),
            stderr_ring: OutputRing::new(self.ring_bytes),
            child: Some(child),
        }));

        state.tasks.insert(id.clone(), Arc::clone(&entry));
        state.order.push(id.clone());
        drop(state);

        tokio::spawn(drain_stream(
            stdout,
            Arc::clone(&entry),
            id.clone(),
            StreamKind::Stdout,
        ));
        tokio::spawn(drain_stream(
            stderr,
            Arc::clone(&entry),
            id.clone(),
            StreamKind::Stderr,
        ));
        tokio::spawn(watch_task(Arc::clone(&entry), id.clone()));
        Ok(id)
    }

    /// Poll one task: current state plus the retained stdout/stderr rings.
    pub async fn poll(&self, id: &str) -> Result<BackgroundTaskSnapshot> {
        let entry = self.entry(id).await?;
        let entry = entry.lock().expect("background task entry lock");
        Ok(entry.snapshot())
    }

    /// Stop a task: SIGKILL its recorded direct child and reap it.
    ///
    /// Idempotent: stopping an already-terminal task returns its recorded
    /// snapshot without signalling anything. A natural completion that raced
    /// the stop is preserved — whichever transition first left `Running` wins,
    /// and no later observer overwrites it.
    pub async fn stop(&self, id: &str) -> Result<BackgroundTaskSnapshot> {
        let entry_arc = self.entry(id).await?;
        let child = {
            let mut entry = entry_arc.lock().expect("background task entry lock");
            if !entry.state.is_running() {
                return Ok(entry.snapshot());
            }
            entry.child.take()
        };

        let Some(mut child) = child else {
            anyhow::bail!("A stop is already in progress for background task {id}");
        };
        if let Err(error) = child.start_kill() {
            tracing::warn!(task = %id, %error, "background task kill failed");
        }
        let status = child
            .wait()
            .await
            .with_context(|| format!("Failed to reap background task {id}"))?;

        let mut entry = entry_arc.lock().expect("background task entry lock");
        // Exactly-once: record only if nothing transitioned meanwhile.
        if entry.state.is_running() {
            entry.state = BackgroundTaskState::Stopped(ExitOutcome::from_status(&status));
        }
        Ok(entry.snapshot())
    }

    /// Kill and reap every running task. This is the daemon-death path: the
    /// owning process is going away, so nothing may survive it.
    pub async fn shutdown_all(&self) {
        let ids: Vec<BackgroundTaskId> = {
            let state = self.state.lock().expect("background manager state lock");
            state.tasks.keys().cloned().collect()
        };
        for id in ids {
            let _ = self.stop(id.as_str()).await;
        }
    }

    async fn entry(&self, id: &str) -> Result<Arc<Mutex<TaskEntry>>> {
        let state = self.state.lock().expect("background manager state lock");
        state
            .tasks
            .get(&BackgroundTaskId(id.to_string()))
            .cloned()
            .ok_or_else(|| anyhow!("No such background task: {id}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
}

fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ExitOutcome {
    fn from_status(status: &std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            if let Some(signal) = status.signal() {
                return Self::Signal(signal as i32);
            }
        }
        match status.code() {
            Some(code) => Self::Code(code),
            // No code and no signal: an unknown-exit fallthrough (only
            // reachable on platforms without the signal extension above).
            None => Self::Code(-1),
        }
    }
}

/// Drain one output stream into its ring, for the life of the task.
async fn drain_stream<S: tokio::io::AsyncRead + Unpin>(
    stream: S,
    entry: Arc<Mutex<TaskEntry>>,
    id: BackgroundTaskId,
    kind: StreamKind,
) {
    let mut lines = BufReader::new(stream).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(task = %id, %error, "background task stream read failed");
                break;
            }
        };
        let mut guard = entry.lock().expect("background task entry lock");
        match kind {
            StreamKind::Stdout => guard.stdout_ring.push(line),
            StreamKind::Stderr => guard.stderr_ring.push(line),
        }
    }
}

/// Await one task's exit and record the terminal state exactly once.
///
/// The watcher owns the `Completed` transition; [`BackgroundTaskManager::stop`]
/// owns the `Stopped` one. Once the child is taken by a stop, this watcher
/// exits without writing anything, so a late natural completion can never
/// overwrite a recorded terminal state.
async fn watch_task(entry: Arc<Mutex<TaskEntry>>, id: BackgroundTaskId) {
    loop {
        {
            let mut guard = entry.lock().expect("background task entry lock");
            match guard.child.as_mut() {
                None => break, // child taken by a stop; that path records state
                Some(child) => match child.try_wait() {
                    Ok(Some(status)) => {
                        if guard.state.is_running() {
                            guard.state = BackgroundTaskState::Completed(status.code());
                        }
                        guard.child = None;
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(task = %id, %error, "background task wait failed");
                        if guard.state.is_running() {
                            guard.state = BackgroundTaskState::Completed(None);
                        }
                        guard.child = None;
                        break;
                    }
                },
            }
        }
        tokio::time::sleep(WATCHER_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests;
