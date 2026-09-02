//! VM-facing binding for the structured child-agent scheduler.

use crate::runtime::scheduler::{
    AgentIdentity, AgentScheduler, AgentTaskResult, AgentTaskSnapshot, AgentTaskSpec,
};
use anyhow::Result;
use std::sync::{Arc, Weak};
use uuid::Uuid;

#[derive(Clone)]
pub struct AgentVmBinding {
    scheduler: Weak<AgentScheduler>,
    parent: Option<AgentIdentity>,
}

impl AgentVmBinding {
    /// `pub(crate)` for the same reason `block_on` is.
    ///
    /// Narrowing only `block_on` left a composed path open: an external crate
    /// could build a binding through the then-`pub` `AgentScheduling::new` and
    /// `AgentVmBinding::new`, attach it to the Co-Forth interpreter's
    /// `set_agent_binding`, and evaluate `agent-await` from a runtime worker --
    /// reaching `block_on_host` with no `spawn_blocking` hop, which is exactly
    /// what its doc says cannot happen. #294 has since removed that
    /// interpreter, so that particular third leg is gone; the constructor stays
    /// `pub(crate)` because the claim should hold on its own terms and not on
    /// which consumers happen to exist.
    pub(crate) fn new(scheduler: &Arc<AgentScheduler>, parent: Option<AgentIdentity>) -> Self {
        Self {
            scheduler: Arc::downgrade(scheduler),
            parent,
        }
    }

    fn scheduler(&self) -> Result<Arc<AgentScheduler>> {
        self.scheduler
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("agent scheduler is unavailable"))
    }

    pub async fn spawn(&self, task: String) -> Result<AgentIdentity> {
        self.spawn_spec(AgentTaskSpec {
            task,
            role: Default::default(),
            background: None,
            provider: None,
            model: None,
            context: Vec::new(),
            capability_grant_ids: None,
            budget: Default::default(),
        })
        .await
    }

    pub async fn spawn_spec(&self, spec: AgentTaskSpec) -> Result<AgentIdentity> {
        self.scheduler()?.spawn(spec, self.parent.as_ref()).await
    }

    pub async fn poll(&self, task_id: Uuid) -> Result<AgentTaskSnapshot> {
        let scheduler = self.scheduler()?;
        scheduler.authorize(task_id, self.parent.as_ref()).await?;
        scheduler.poll(task_id).await
    }

    pub async fn wait(&self, task_id: Uuid) -> Result<AgentTaskResult> {
        let scheduler = self.scheduler()?;
        scheduler.authorize(task_id, self.parent.as_ref()).await?;
        scheduler.wait(task_id).await
    }

    pub async fn cancel(&self, task_id: Uuid) -> Result<()> {
        let scheduler = self.scheduler()?;
        scheduler.authorize(task_id, self.parent.as_ref()).await?;
        scheduler.cancel(task_id).await
    }

    /// Run an async scheduler effect to completion from synchronous typed code.
    ///
    /// **Callers must not be on a runtime worker thread**, for the reason
    /// `super::block_on_host` documents at length: the join blocks the calling
    /// thread, and `agent-await` waits on a child task that is `tokio::spawn`ed
    /// onto the same runtime, so on a single-worker runtime the worker the
    /// child needs is the one blocking.
    ///
    /// This delegates rather than repeating the shape. It was byte-for-byte
    /// `block_on_host` with none of the explanation, so when #284 established
    /// and documented the constraint on one of them, the other kept carrying it
    /// silently (#289). One implementation is one place to state the rule, and
    /// one place to change if the rule ever stops holding.
    /// `pub(crate)`, deliberately. `block_on_host`'s doc rests on the claim
    /// that an out-of-tree caller cannot reach it, and a `pub` wrapper that
    /// delegates there would have made that claim false -- the delegation would
    /// have widened the reachable surface rather than only sharing the
    /// explanation. Every caller is in-crate.
    pub(crate) fn block_on<T: Send + 'static>(
        &self,
        operation: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        super::block_on_host(operation)
    }
}

pub fn parse_task_id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| anyhow::anyhow!("invalid agent task id: {error}"))
}
