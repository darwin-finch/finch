// Brain Registry — tracks daemon-side brain sessions
//
// Each brain session runs as a tokio task, intercepts AskUserQuestion and
// PresentPlan tool calls, and communicates back to the REPL via polling.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{oneshot, RwLock};
use uuid::Uuid;

/// Convert a task description into a URL-safe slug.
///
/// Takes the first 4 words, lowercases, joins with `-`, strips non-alphanumeric.
pub fn slug_task(task: &str) -> String {
    let words: Vec<String> = task
        .split_whitespace()
        .take(4)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect();
    if words.is_empty() {
        "brain".to_string()
    } else {
        words.join("-")
    }
}

/// State of a daemon brain session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainState {
    Running,
    WaitingForInput,
    PlanReady,
    Dead,
}

/// A pending user question from the brain.
pub struct PendingQuestion {
    pub question: String,
    pub options: Vec<String>,
    pub response_tx: oneshot::Sender<String>,
}

/// A pending plan from the brain.
pub struct PendingPlan {
    pub plan: String,
    pub response_tx: oneshot::Sender<PlanResponse>,
}

/// The user's response to a brain plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum PlanResponse {
    Approve,
    ChangesRequested { feedback: String },
    Reject,
}

/// A single brain session entry in the registry.
pub struct BrainEntry {
    pub id: Uuid,
    /// Auto-generated slug from the task (e.g. "investigate-why-auth-tests")
    pub name: String,
    pub task: String,
    pub state: BrainState,
    /// Append-only log of brain output (shown via /brains attach)
    pub event_log: Vec<String>,
    pub pending_question: Option<PendingQuestion>,
    pub pending_plan: Option<PendingPlan>,
    pub created_at: Instant,
}

/// Serializable summary of a brain (for GET /v1/brains list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainSummary {
    pub id: Uuid,
    pub name: String,
    pub task: String,
    pub state: BrainState,
    pub age_secs: u64,
}

/// Full detail of a brain (for GET /v1/brains/:id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainDetail {
    pub id: Uuid,
    pub name: String,
    pub task: String,
    pub state: BrainState,
    pub event_log: Vec<String>,
    pub pending_question: Option<PendingQuestionView>,
    pub pending_plan: Option<PendingPlanView>,
    pub age_secs: u64,
}

/// Serializable view of a pending question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestionView {
    pub question: String,
    pub options: Vec<String>,
}

/// Serializable view of a pending plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPlanView {
    pub plan: String,
}

impl BrainEntry {
    pub fn to_summary(&self) -> BrainSummary {
        BrainSummary {
            id: self.id,
            name: self.name.clone(),
            task: self.task.clone(),
            state: self.state.clone(),
            age_secs: self.created_at.elapsed().as_secs(),
        }
    }

    pub fn to_detail(&self) -> BrainDetail {
        BrainDetail {
            id: self.id,
            name: self.name.clone(),
            task: self.task.clone(),
            state: self.state.clone(),
            event_log: self.event_log.clone(),
            pending_question: self.pending_question.as_ref().map(|q| PendingQuestionView {
                question: q.question.clone(),
                options: q.options.clone(),
            }),
            pending_plan: self.pending_plan.as_ref().map(|p| PendingPlanView {
                plan: p.plan.clone(),
            }),
            age_secs: self.created_at.elapsed().as_secs(),
        }
    }
}

/// Thread-safe registry of all daemon brain sessions.
#[derive(Clone)]
pub struct BrainRegistry {
    brains: Arc<RwLock<HashMap<Uuid, BrainEntry>>>,
    /// Name → id for deduplication
    names: Arc<RwLock<HashMap<String, Uuid>>>,
}

impl BrainRegistry {
    pub fn new() -> Self {
        Self {
            brains: Arc::new(RwLock::new(HashMap::new())),
            names: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert a new entry, returning the assigned name (slug, with collision suffix).
    pub async fn insert(&self, id: Uuid, task: String) -> String {
        let base = slug_task(&task);
        let mut names = self.names.write().await;
        let mut brains = self.brains.write().await;

        // Resolve collision: base → base-2 → base-3 ...
        let name = if !names.contains_key(&base) {
            base.clone()
        } else {
            let mut n = 2;
            loop {
                let candidate = format!("{}-{}", base, n);
                if !names.contains_key(&candidate) {
                    break candidate;
                }
                n += 1;
            }
        };

        names.insert(name.clone(), id);
        brains.insert(
            id,
            BrainEntry {
                id,
                name: name.clone(),
                task,
                state: BrainState::Running,
                event_log: Vec::new(),
                pending_question: None,
                pending_plan: None,
                created_at: Instant::now(),
            },
        );

        name
    }

    /// Append a line to the brain's event log.
    pub async fn append_log(&self, id: Uuid, line: String) {
        let mut brains = self.brains.write().await;
        if let Some(entry) = brains.get_mut(&id) {
            entry.event_log.push(line);
        }
    }

    /// Transition to WaitingForInput and store the pending question.
    pub async fn set_waiting_for_input(
        &self,
        id: Uuid,
        question: String,
        options: Vec<String>,
        response_tx: oneshot::Sender<String>,
    ) {
        let mut brains = self.brains.write().await;
        if let Some(entry) = brains.get_mut(&id) {
            entry.state = BrainState::WaitingForInput;
            entry.pending_question = Some(PendingQuestion {
                question,
                options,
                response_tx,
            });
        }
    }

    /// Transition to PlanReady and store the pending plan.
    pub async fn set_plan_ready(
        &self,
        id: Uuid,
        plan: String,
        response_tx: oneshot::Sender<PlanResponse>,
    ) {
        let mut brains = self.brains.write().await;
        if let Some(entry) = brains.get_mut(&id) {
            entry.state = BrainState::PlanReady;
            entry.pending_plan = Some(PendingPlan { plan, response_tx });
        }
    }

    /// Mark a brain as dead.
    pub async fn set_dead(&self, id: Uuid) {
        let mut brains = self.brains.write().await;
        if let Some(entry) = brains.get_mut(&id) {
            entry.state = BrainState::Dead;
            entry.pending_question = None;
            entry.pending_plan = None;
        }
    }

    /// Answer a pending question. Returns Err if no question is pending.
    pub async fn answer_question(&self, id: Uuid, answer: String) -> Result<()> {
        let mut brains = self.brains.write().await;
        let entry = brains
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("Brain {} not found", id))?;

        let question = entry
            .pending_question
            .take()
            .ok_or_else(|| anyhow::anyhow!("Brain {} has no pending question", id))?;

        entry.state = BrainState::Running;
        let _ = question.response_tx.send(answer);
        Ok(())
    }

    /// Respond to a pending plan. Returns Err if no plan is pending.
    pub async fn respond_to_plan(&self, id: Uuid, response: PlanResponse) -> Result<()> {
        let mut brains = self.brains.write().await;
        let entry = brains
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("Brain {} not found", id))?;

        let plan = entry
            .pending_plan
            .take()
            .ok_or_else(|| anyhow::anyhow!("Brain {} has no pending plan", id))?;

        // If approved/rejected → go Dead after response; if changes requested → Running
        entry.state = match &response {
            PlanResponse::ChangesRequested { .. } => BrainState::Running,
            _ => BrainState::Dead,
        };

        let _ = plan.response_tx.send(response);
        Ok(())
    }

    /// Cancel (remove) a brain by ID. Returns true if found.
    pub async fn cancel(&self, id: Uuid) -> bool {
        let mut brains = self.brains.write().await;
        if let Some(entry) = brains.remove(&id) {
            // Also remove from name index
            let mut names = self.names.write().await;
            names.remove(&entry.name);
            true
        } else {
            false
        }
    }

    /// List all non-dead brains as summaries.
    pub async fn list_active(&self) -> Vec<BrainSummary> {
        let brains = self.brains.read().await;
        let mut result: Vec<BrainSummary> = brains
            .values()
            .filter(|e| e.state != BrainState::Dead)
            .map(|e| e.to_summary())
            .collect();
        result.sort_by_key(|s| s.age_secs);
        result
    }

    /// List all brains (including dead) as summaries.
    pub async fn list_all(&self) -> Vec<BrainSummary> {
        let brains = self.brains.read().await;
        let mut result: Vec<BrainSummary> = brains.values().map(|e| e.to_summary()).collect();
        result.sort_by_key(|s| s.age_secs);
        result
    }

    /// Get full detail for a brain.
    pub async fn get_detail(&self, id: Uuid) -> Option<BrainDetail> {
        let brains = self.brains.read().await;
        brains.get(&id).map(|e| e.to_detail())
    }

    /// Lookup brain ID by name.
    pub async fn id_by_name(&self, name: &str) -> Option<Uuid> {
        let names = self.names.read().await;
        names.get(name).copied()
    }
}

impl Default for BrainRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_task_basic() {
        assert_eq!(slug_task("investigate why auth tests"), "investigate-why-auth-tests");
    }

    #[test]
    fn test_slug_task_strips_non_alphanumeric() {
        assert_eq!(slug_task("fix the 'broken' auth!"), "fix-the-broken-auth");
    }

    #[test]
    fn test_slug_task_caps_at_four_words() {
        assert_eq!(
            slug_task("investigate why auth tests are flaky now"),
            "investigate-why-auth-tests"
        );
    }

    #[test]
    fn test_slug_task_empty() {
        assert_eq!(slug_task(""), "brain");
    }

    #[test]
    fn test_slug_task_single_word() {
        assert_eq!(slug_task("cargo"), "cargo");
    }

    #[tokio::test]
    async fn test_registry_insert_and_list() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        let name = registry.insert(id, "investigate auth tests".to_string()).await;
        assert_eq!(name, "investigate-auth-tests");

        let list = registry.list_active().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].name, "investigate-auth-tests");
    }

    #[tokio::test]
    async fn test_registry_collision_handling() {
        let registry = BrainRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let name1 = registry.insert(id1, "investigate auth tests".to_string()).await;
        let name2 = registry.insert(id2, "investigate auth tests".to_string()).await;
        assert_eq!(name1, "investigate-auth-tests");
        assert_eq!(name2, "investigate-auth-tests-2");
    }

    #[tokio::test]
    async fn test_answer_question() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        registry.insert(id, "test task".to_string()).await;

        let (tx, rx) = oneshot::channel();
        registry
            .set_waiting_for_input(id, "question?".to_string(), vec![], tx)
            .await;

        {
            let brains = registry.brains.read().await;
            assert_eq!(brains[&id].state, BrainState::WaitingForInput);
        }

        registry.answer_question(id, "yes".to_string()).await.unwrap();
        assert_eq!(rx.await.unwrap(), "yes");

        {
            let brains = registry.brains.read().await;
            assert_eq!(brains[&id].state, BrainState::Running);
        }
    }

    #[tokio::test]
    async fn test_respond_to_plan_approve() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        registry.insert(id, "test task".to_string()).await;

        let (tx, rx) = oneshot::channel();
        registry
            .set_plan_ready(id, "do this".to_string(), tx)
            .await;

        registry
            .respond_to_plan(id, PlanResponse::Approve)
            .await
            .unwrap();
        assert!(matches!(rx.await.unwrap(), PlanResponse::Approve));

        let brains = registry.brains.read().await;
        assert_eq!(brains[&id].state, BrainState::Dead);
    }

    #[tokio::test]
    async fn test_respond_to_plan_changes_requested_keeps_running() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        registry.insert(id, "test task".to_string()).await;

        let (tx, _rx) = oneshot::channel();
        registry
            .set_plan_ready(id, "do this".to_string(), tx)
            .await;

        registry
            .respond_to_plan(
                id,
                PlanResponse::ChangesRequested {
                    feedback: "add tests".to_string(),
                },
            )
            .await
            .unwrap();

        let brains = registry.brains.read().await;
        assert_eq!(brains[&id].state, BrainState::Running);
    }

    #[tokio::test]
    async fn test_cancel_removes_brain() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        registry.insert(id, "test task".to_string()).await;

        assert!(registry.cancel(id).await);
        assert!(registry.list_all().await.is_empty());
        assert!(!registry.cancel(id).await);
    }

    #[tokio::test]
    async fn test_id_by_name() {
        let registry = BrainRegistry::new();
        let id = Uuid::new_v4();
        let name = registry.insert(id, "find the bug".to_string()).await;
        assert_eq!(registry.id_by_name(&name).await, Some(id));
        assert_eq!(registry.id_by_name("nonexistent").await, None);
    }

    #[tokio::test]
    async fn test_list_active_excludes_dead() {
        let registry = BrainRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        registry.insert(id1, "running task".to_string()).await;
        registry.insert(id2, "dead task".to_string()).await;
        registry.set_dead(id2).await;

        let active = registry.list_active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id1);
    }
}
