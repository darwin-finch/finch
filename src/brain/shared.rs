//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

/// The one machine/workspace boundary in which a brain may cause effects.
///
/// There is deliberately no separate `execution_head`: the machine that owns
/// the workspace is the only machine allowed to execute the brain's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainEnvironment {
    pub machine: String,
    pub workspace: PathBuf,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainEventKind {
    Prompt {
        text: String,
    },
    Program {
        language: ProgramLanguage,
        source: String,
    },
    ProgramPopped {
        program_seq: u64,
    },
    Result {
        request_seq: u64,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Content-addressed typed-VM state committed after one accepted program.
    /// This is an internal Brain event, not a request to replay source after
    /// restart; the checkpoint bytes live beside the append-only log.
    RuntimeCommitted {
        request_seq: u64,
        runtime_revision: u64,
        checkpoint_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainEvent {
    pub seq: u64,
    /// Binds this event to the exact environment revision in which it ran.
    #[serde(default = "initial_environment_generation")]
    pub environment_generation: u64,
    pub sender: String,
    pub created_ms: u64,
    #[serde(flatten)]
    pub kind: BrainEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainProgram {
    pub seq: u64,
    pub sender: String,
    pub language: ProgramLanguage,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    pub name: String,
    pub environment: BrainEnvironment,
    pub revision: u64,
    pub events: Vec<BrainEvent>,
    pub program_stack: Vec<BrainProgram>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrainWireMessage {
    Snapshot { brain: BrainSnapshot },
    Event { event: BrainEvent },
}

struct BrainState {
    events: Vec<BrainEvent>,
    program_stack: Vec<BrainProgram>,
    runtime_checkpoint: Option<RuntimeCheckpointState>,
    runtime_commit_count: u64,
    tx: broadcast::Sender<BrainEvent>,
}

#[derive(Debug, Clone)]
struct RuntimeCheckpointState {
    request_seq: u64,
    durable_revision: u64,
    checkpoint_sha256: String,
}

impl BrainState {
    fn from_events(events: Vec<BrainEvent>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut state = Self {
            events: Vec::new(),
            program_stack: Vec::new(),
            runtime_checkpoint: None,
            runtime_commit_count: 0,
            tx,
        };
        for event in events {
            state.apply(event);
        }
        state
    }

    fn apply(&mut self, event: BrainEvent) {
        match &event.kind {
            BrainEventKind::Program { language, source } => {
                self.program_stack.push(BrainProgram {
                    seq: event.seq,
                    sender: event.sender.clone(),
                    language: *language,
                    source: source.clone(),
                });
            }
            BrainEventKind::ProgramPopped { program_seq } => {
                if self.program_stack.last().map(|p| p.seq) == Some(*program_seq) {
                    self.program_stack.pop();
                }
            }
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision,
                checkpoint_sha256,
            } => {
                self.runtime_commit_count += 1;
                let durable_revision = self
                    .runtime_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.durable_revision)
                    .unwrap_or(0)
                    .max(*runtime_revision)
                    .max(self.runtime_commit_count);
                if self.runtime_checkpoint.as_ref().is_none_or(|current| {
                    request_seq >= &current.request_seq
                }) {
                    self.runtime_checkpoint = Some(RuntimeCheckpointState {
                        request_seq: *request_seq,
                        durable_revision,
                        checkpoint_sha256: checkpoint_sha256.clone(),
                    });
                } else if let Some(current) = self.runtime_checkpoint.as_mut() {
                    current.durable_revision = durable_revision;
                }
            }
            BrainEventKind::Prompt { .. } | BrainEventKind::Result { .. } => {}
        }
        self.events.push(event);
    }
}

/// Persistent registry of named shared brains.
///
/// Each brain is stored as human-browsable JSON Lines under
/// `~/.finch/brains/<name>/events.jsonl`.  The log is authoritative; the
/// program stack is rebuilt from it after a daemon restart.
#[derive(Clone)]
pub struct SharedBrainStore {
    root: Option<PathBuf>,
    environment: BrainEnvironment,
    brains: Arc<RwLock<HashMap<String, BrainState>>>,
    runtimes: Arc<RwLock<HashMap<String, Arc<crate::runtime::ProgramRuntime>>>>,
    runtime_checkpoints:
        Arc<RwLock<HashMap<String, crate::vm::TypedRuntimeCheckpoint>>>,
    /// One ordered turn lane per Brain. HTTP/WebSocket clients may submit
    /// concurrently, but accepted input, VM commit, and its Result event must
    /// remain an indivisible sequence against the authoritative revision.
    execution_locks: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl SharedBrainStore {
    pub fn new(machine: impl Into<String>) -> Self {
        let root = dirs::home_dir().map(|p| p.join(".finch").join("brains"));
        Self::with_root(machine, root)
    }

    pub fn with_root(machine: impl Into<String>, root: Option<PathBuf>) -> Self {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_environment(machine, workspace, root)
    }

    pub fn with_environment(
        machine: impl Into<String>,
        workspace: impl Into<PathBuf>,
        root: Option<PathBuf>,
    ) -> Self {
        let workspace = workspace.into();
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        Self {
            root,
            environment: BrainEnvironment {
                machine: machine.into(),
                workspace,
                generation: initial_environment_generation(),
            },
            brains: Arc::new(RwLock::new(HashMap::new())),
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            runtime_checkpoints: Arc::new(RwLock::new(HashMap::new())),
            execution_locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn environment(&self) -> &BrainEnvironment {
        &self.environment
    }

    pub(crate) fn execution_lock(&self, name: &str) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let name = Self::validate_name(name)?;
        if let Some(lock) = self
            .execution_locks
            .read()
            .expect("shared brain execution-lock map poisoned")
            .get(name)
            .cloned()
        {
            return Ok(lock);
        }
        let mut locks = self
            .execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned");
        Ok(locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    pub fn validate_name(name: &str) -> Result<&str> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            anyhow::bail!("brain name must use 1-64 letters, numbers, '-' or '_'");
        }
        Ok(name)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        self.load_all()?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let mut names: Vec<_> = brains.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    pub fn snapshot(&self, name: &str) -> Result<BrainSnapshot> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).expect("brain loaded above");
        Ok(BrainSnapshot {
            name: name.to_string(),
            environment: self.environment.clone(),
            revision: state.events.last().map(|e| e.seq).unwrap_or(0),
            events: state.events.clone(),
            program_stack: state.program_stack.clone(),
        })
    }

    pub fn push(&self, name: &str, sender: &str, kind: BrainEventKind) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let event = BrainEvent {
            seq: state.events.last().map(|e| e.seq + 1).unwrap_or(1),
            environment_generation: self.environment.generation,
            sender: sender.trim().to_string(),
            created_ms: unix_millis(),
            kind,
        };
        self.append_event(name, &event)?;
        state.apply(event.clone());
        let _ = state.tx.send(event.clone());
        Ok(event)
    }

    pub fn pop_program(&self, name: &str, sender: &str) -> Result<Option<BrainEvent>> {
        let snapshot = self.snapshot(name)?;
        let Some(program) = snapshot.program_stack.last() else {
            return Ok(None);
        };
        self.push(
            name,
            sender,
            BrainEventKind::ProgramPopped {
                program_seq: program.seq,
            },
        )
        .map(Some)
    }

    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<BrainEvent>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        Ok(brains.get(name).expect("brain loaded above").tx.subscribe())
    }

    /// Return the one live typed runtime for a named Brain, restoring its
    /// latest reducible checkpoint on first access after daemon restart.
    pub fn program_runtime(&self, name: &str) -> Result<Arc<crate::runtime::ProgramRuntime>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(runtime) = self
            .runtimes
            .read()
            .expect("shared brain runtime lock poisoned")
            .get(name)
            .cloned()
        {
            return Ok(runtime);
        }
        let checkpoint = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.clone());
        let runtime = Arc::new(match checkpoint {
            Some(checkpoint) => {
                crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
                    self.read_runtime_checkpoint(name, &checkpoint.checkpoint_sha256)?,
                    checkpoint.durable_revision,
                )?
            }
            None => crate::runtime::ProgramRuntime::new(),
        });
        let mut runtimes = self
            .runtimes
            .write()
            .expect("shared brain runtime lock poisoned");
        Ok(runtimes
            .entry(name.to_string())
            .or_insert_with(|| Arc::clone(&runtime))
            .clone())
    }

    /// Journal the latest checkpoint only after a ProgramRuntime commit. The
    /// source event remains the audit record; restart restores state rather
    /// than replaying effects from that source.
    pub fn commit_runtime(
        &self,
        name: &str,
        request_seq: u64,
        runtime_revision: u64,
        runtime: &crate::runtime::ProgramRuntime,
    ) -> Result<BrainEvent> {
        let snapshot = runtime
            .revision_history()?
            .into_iter()
            .find(|snapshot| snapshot.revision == runtime_revision)
            .with_context(|| {
                format!("typed runtime has no revision snapshot {runtime_revision}")
            })?;
        let checkpoint = snapshot.checkpoint.context(
            "typed runtime revision contains host-owned handles and cannot be persisted yet",
        )?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        self.write_runtime_checkpoint(name, &checkpoint_sha256, &encoded)?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.clone(), checkpoint);
        self.push(
            name,
            "daemon",
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision: snapshot.revision,
                checkpoint_sha256,
            },
        )
    }

    fn read_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
    ) -> Result<crate::vm::TypedRuntimeCheckpoint> {
        if let Some(checkpoint) = self
            .runtime_checkpoints
            .read()
            .expect("shared brain checkpoint lock poisoned")
            .get(checkpoint_sha256)
            .cloned()
        {
            return Ok(checkpoint);
        }
        let root = self
            .root
            .as_ref()
            .context("named Brain checkpoint is not available in this process")?;
        let path = root
            .join(name)
            .join("runtime")
            .join(format!("{checkpoint_sha256}.json"));
        let encoded = std::fs::read(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let actual = hex::encode(Sha256::digest(&encoded));
        if actual != checkpoint_sha256 {
            anyhow::bail!("typed runtime checkpoint hash mismatch for {checkpoint_sha256}");
        }
        let checkpoint: crate::vm::TypedRuntimeCheckpoint = serde_json::from_slice(&encoded)
            .with_context(|| format!("parse {}", path.display()))?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.to_string(), checkpoint.clone());
        Ok(checkpoint)
    }

    fn write_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
        encoded: &[u8],
    ) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let directory = root.join(name).join("runtime");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(format!("{checkpoint_sha256}.json"));
        if path.exists() {
            return Ok(());
        }
        let temporary = directory.join(format!(
            ".{checkpoint_sha256}.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    fn ensure_loaded(&self, name: &str) -> Result<()> {
        if self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .contains_key(name)
        {
            return Ok(());
        }
        let events = self.read_events(name)?;
        // Preserve an empty named Brain across daemon restarts, even before
        // its first conversational event is appended.
        if let Some(root) = &self.root {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
        }
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .entry(name.to_string())
            .or_insert_with(|| BrainState::from_events(events));
        Ok(())
    }

    fn load_all(&self) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    if Self::validate_name(name).is_ok() {
                        self.ensure_loaded(name)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn event_path(&self, name: &str) -> Option<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(name).join("events.jsonl"))
    }

    fn read_events(&self, name: &str) -> Result<Vec<BrainEvent>> {
        let Some(path) = self.event_path(name) else {
            return Ok(Vec::new());
        };
        let Ok(file) = std::fs::File::open(&path) else {
            return Ok(Vec::new());
        };
        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_no, line)| match line {
                Ok(line) if line.trim().is_empty() => None,
                other => Some((line_no, other)),
            })
            .map(|(line_no, line)| {
                let line = line.with_context(|| format!("read {}", path.display()))?;
                serde_json::from_str(&line)
                    .with_context(|| format!("parse {} line {}", path.display(), line_no + 1))
            })
            .collect()
    }

    fn append_event(&self, name: &str, event: &BrainEvent) -> Result<()> {
        let Some(path) = self.event_path(name) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const fn initial_environment_generation() -> u64 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_stack_is_rebuilt_from_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let first = store
            .push(
                "finch",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Forth,
                    source: "2 3 +".into(),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "bob",
                BrainEventKind::Prompt {
                    text: "explain that".into(),
                },
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("finch").unwrap();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.program_stack.len(), 1);
        assert_eq!(snapshot.program_stack[0].seq, first.seq);
        assert_eq!(snapshot.environment.machine, "workstation.local");
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn pop_is_an_event_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        store
            .push(
                "brain",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "(+ 1 2)".into(),
                },
            )
            .unwrap();
        let popped = store.pop_program("brain", "alice").unwrap().unwrap();
        assert!(matches!(popped.kind, BrainEventKind::ProgramPopped { .. }));

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted
            .snapshot("brain")
            .unwrap()
            .program_stack
            .is_empty());
    }

    #[test]
    fn subscribers_receive_the_authoritative_sequence() {
        let store = SharedBrainStore::with_root("box.local", None);
        let mut first = store.subscribe("brain").unwrap();
        let mut second = store.subscribe("brain").unwrap();
        let event = store
            .push(
                "brain",
                "alice",
                BrainEventKind::Prompt { text: "hi".into() },
            )
            .unwrap();
        assert_eq!(first.try_recv().unwrap(), event);
        assert_eq!(second.try_recv().unwrap(), event);
    }

    #[test]
    fn empty_named_brain_remains_listed_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        store.snapshot("quiet-brain").unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.list().unwrap(), vec!["quiet-brain"]);
        assert_eq!(restarted.snapshot("quiet-brain").unwrap().revision, 0);
    }

    #[tokio::test]
    async fn attached_clients_share_one_ordered_turn_lane_per_brain() {
        let store = SharedBrainStore::with_root("box.local", None);
        let first = store.execution_lock("brain").unwrap();
        let same_brain = store.execution_lock("brain").unwrap();
        let other_brain = store.execution_lock("other").unwrap();
        assert!(Arc::ptr_eq(&first, &same_brain));
        assert!(!Arc::ptr_eq(&first, &other_brain));

        let first_turn = first.lock_owned().await;
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let waiting = tokio::spawn(async move {
            let _second_turn = same_brain.lock_owned().await;
            entered_tx.send(()).unwrap();
        });
        tokio::task::yield_now().await;
        assert!(entered_rx.try_recv().is_err());

        drop(first_turn);
        entered_rx.recv().await.unwrap();
        waiting.await.unwrap();
    }

    #[tokio::test]
    async fn named_brain_restores_one_typed_runtime_without_replaying_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("brain:event:1".into()),
                source: ": square ( S n:int -- S int ! pure ) n n * ;".into(),
                intent: "define square".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        let committed_revision = outcome.output_revision;
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|revision| revision.checkpoint.clone())
            .unwrap();
        let encoded_checkpoint = serde_json::to_string(&checkpoint).unwrap();
        serde_json::from_str::<crate::vm::TypedRuntimeCheckpoint>(&encoded_checkpoint)
            .expect("checkpoint itself must round-trip through JSON");
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();

        let event_log = std::fs::read_to_string(temp.path().join("brain/events.jsonl")).unwrap();
        for line in event_log.lines() {
            if let Err(error) = serde_json::from_str::<BrainEvent>(line) {
                panic!("checkpoint event must round-trip through JSONL: {error}\n{line}");
            }
        }

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), committed_revision);
        let outcome = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("brain:event:2".into()),
                source: "(square 7)".into(),
                intent: "call restored definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.values, vec![crate::programs::ProgramValue::Int(49)]);
        assert_eq!(outcome.output_revision, committed_revision + 1);
    }

    #[tokio::test]
    async fn out_of_order_checkpoint_events_never_regress_a_brain_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "concurrent checkpoint ordering".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime.submit_typed_only(submit("1", 0)).await.unwrap();
        let second = runtime.submit_typed_only(submit("2", 1)).await.unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), second.output_revision);
        let values = restored
            .inspect()
            .await
            .unwrap()
            .stack
            .into_iter()
            .map(|cell| cell.value)
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                crate::programs::ProgramValue::Int(1),
                crate::programs::ProgramValue::Int(2),
            ]
        );
    }

    #[tokio::test]
    async fn legacy_restart_revision_reset_keeps_the_latest_request_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = crate::runtime::ProgramRuntime::new();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "legacy revision migration".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime
            .submit_typed_only(submit(
                ": square ( S n:int -- S int ! pure ) n n * ;",
                0,
            ))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();
        let second = runtime
            .submit_typed_only(submit("1 drop", first.output_revision))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();

        // ProgramRuntime::from_checkpoint historically reset its local
        // revision. Simulate an old daemon adding newer state as revision 1.
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|snapshot| snapshot.checkpoint.clone())
            .unwrap();
        let legacy_restarted = crate::runtime::ProgramRuntime::from_checkpoint(checkpoint).unwrap();
        let legacy_commit = legacy_restarted
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: ": cube ( S n:int -- S int ! pure ) n n * n * ;".into(),
                intent: "new state after legacy restart".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: legacy_restarted.manifest_generation(),
                expected_revision: Some(0),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(legacy_commit.output_revision, 1);
        store
            .commit_runtime(
                "brain",
                3,
                legacy_commit.output_revision,
                &legacy_restarted,
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), 3);
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: "(cube 4)".into(),
                intent: "call latest migrated definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(3),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(64)]);
        assert_eq!(called.output_revision, 4);
    }

    #[test]
    fn names_cannot_escape_the_storage_root() {
        assert!(SharedBrainStore::validate_name("../other").is_err());
        assert!(SharedBrainStore::validate_name("valid-brain_2").is_ok());
    }

    #[test]
    // An environment is an indivisible authority boundary, not two routable heads.
    fn environment_binds_machine_and_workspace_as_one_revision() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_environment(
            "gpu-box.local",
            workspace.path(),
            Some(state.path().into()),
        );

        store
            .push(
                "project",
                "laptop.local",
                BrainEventKind::Prompt { text: "go".into() },
            )
            .unwrap();
        let snapshot = store.snapshot("project").unwrap();

        assert_eq!(snapshot.environment.machine, "gpu-box.local");
        assert_eq!(
            snapshot.environment.workspace,
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(snapshot.environment.generation, 1);
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn old_events_default_to_the_initial_environment_generation() {
        let event: BrainEvent = serde_json::from_str(
            r#"{"seq":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hi"}"#,
        )
        .unwrap();
        assert_eq!(event.environment_generation, 1);
    }
}
