/// Co-Forth session management — mutual stack construction with consensus execution.
///
/// Two peers build programs for each other via `yield`.  Neither executes until
/// both call `agree`.  When consensus is reached the scheduler fires both programs
/// simultaneously on their respective targets.
///
/// Message protocol (minimal):
///   Push(program)  — yield a program fragment for the other peer
///   Agree          — signal readiness to execute
///   Execute        — triggered automatically when both peers agree
///   Get            — poll session state
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// A co-forth session between two peers.
#[derive(Debug, Serialize, Deserialize)]
pub struct CoForthSession {
    pub id: Uuid,
    pub peer_a: String,
    pub peer_b: String,
    /// Program fragments peer_a yielded (intended for peer_b to run).
    pub stack_a: Vec<String>,
    /// Program fragments peer_b yielded (intended for peer_a to run).
    pub stack_b: Vec<String>,
    pub agreed_a: bool,
    pub agreed_b: bool,
}

impl CoForthSession {
    pub fn new(peer_a: String, peer_b: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            peer_a,
            peer_b,
            stack_a: Vec::new(),
            stack_b: Vec::new(),
            agreed_a: false,
            agreed_b: false,
        }
    }

    /// Push a program fragment from `from` onto their outbound stack.
    pub fn push_yield(&mut self, from: &str, program: String) {
        if from == self.peer_a {
            self.stack_a.push(program);
        } else {
            self.stack_b.push(program);
        }
    }

    /// Signal that `from` agrees — ready to execute.
    pub fn agree(&mut self, from: &str) {
        if from == self.peer_a {
            self.agreed_a = true;
        } else {
            self.agreed_b = true;
        }
    }

    /// Both peers have agreed — consensus reached, safe to execute both stacks.
    pub fn consensus(&self) -> bool {
        self.agreed_a && self.agreed_b
    }

    /// The program peer_b should run (what peer_a yielded).
    pub fn program_for_b(&self) -> String {
        self.stack_a.join(" ")
    }

    /// The program peer_a should run (what peer_b yielded).
    pub fn program_for_a(&self) -> String {
        self.stack_b.join(" ")
    }
}

/// Shared session store — all active co-forth sessions indexed by UUID.
pub type SessionStore = Arc<Mutex<HashMap<Uuid, CoForthSession>>>;

pub fn new_session_store() -> SessionStore {
    Arc::new(Mutex::new(HashMap::new()))
}
