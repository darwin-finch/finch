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
    pub fn new(scheduler: &Arc<AgentScheduler>, parent: Option<AgentIdentity>) -> Self {
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

    pub fn block_on<T: Send + 'static>(
        &self,
        operation: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("agent scheduler requires a Tokio runtime"))?;
        std::thread::scope(|scope| {
            scope
                .spawn(move || handle.block_on(operation))
                .join()
                .map_err(|_| anyhow::anyhow!("agent scheduler worker panicked"))?
        })
    }
}

pub fn parse_task_id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| anyhow::anyhow!("invalid agent task id: {error}"))
}
