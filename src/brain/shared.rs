//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainEvent {
    pub seq: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    pub name: String,
    pub machine: String,
    pub revision: u64,
    pub events: Vec<BrainEvent>,
    pub program_stack: Vec<BrainProgram>,
}

struct BrainState {
    events: Vec<BrainEvent>,
    program_stack: Vec<BrainProgram>,
    tx: broadcast::Sender<BrainEvent>,
}

impl BrainState {
    fn from_events(events: Vec<BrainEvent>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut state = Self {
            events: Vec::new(),
            program_stack: Vec::new(),
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
    machine: String,
    brains: Arc<RwLock<HashMap<String, BrainState>>>,
}

impl SharedBrainStore {
    pub fn new(machine: impl Into<String>) -> Self {
        let root = dirs::home_dir().map(|p| p.join(".finch").join("brains"));
        Self::with_root(machine, root)
    }

    pub fn with_root(machine: impl Into<String>, root: Option<PathBuf>) -> Self {
        Self {
            root,
            machine: machine.into(),
            brains: Arc::new(RwLock::new(HashMap::new())),
        }
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
            machine: self.machine.clone(),
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
        assert_eq!(snapshot.machine, "workstation.local");
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
        let mut rx = store.subscribe("brain").unwrap();
        let event = store
            .push(
                "brain",
                "alice",
                BrainEventKind::Prompt { text: "hi".into() },
            )
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), event);
    }

    #[test]
    fn names_cannot_escape_the_storage_root() {
        assert!(SharedBrainStore::validate_name("../other").is_err());
        assert!(SharedBrainStore::validate_name("valid-brain_2").is_ok());
    }
}
