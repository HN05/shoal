//! The agents a trigger's launch meets and starts, the agent queue the
//! runner's jobs wait in, and the load they count for at admission.
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::launches::{Launch, LaunchSource};
use shoal_runner::daemon::Agents;

use crate::{
    daemon::{capacity::Activity, manager::Manager},
    model::QueueEntry,
    state::QueueState,
};

impl Agents for Manager {
    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(Manager::queue_entries(self))
    }

    fn live_queue_entries(&self) -> BoxFuture<'_, Result<HashSet<QueueEntryId>>> {
        Box::pin(async move {
            let entries = self.queue_entries().await?;
            Ok((entries.into_iter())
                .filter(|entry| entry.state != QueueState::Failed)
                .map(|entry| entry.id)
                .collect())
        })
    }

    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(Manager::started_launch(self, entry))
    }

    fn wake_admission(&self) {
        self.agent_queue.wake();
    }

    fn queue_suspended(&self) -> bool {
        self.agent_queue.suspended()
    }

    fn readings_recovered(&self) -> bool {
        self.overload_recovery.published().is_some()
    }

    fn lock_admission(&self) -> BoxFuture<'_, Box<dyn Send + '_>> {
        Box::pin(async move {
            let guard: Box<dyn Send + '_> = Box::new(self.agent_queue.lock_admission().await);
            guard
        })
    }

    fn job_could_start(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(Manager::job_could_start(self))
    }

    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async move {
            let mut spaces = self.recorded_agent_spaces().await?;
            spaces.extend(self.queued_agent_workspaces().await?);
            Ok(spaces)
        })
    }

    fn launch(
        self: Arc<Self>,
        launch: Launch,
        source: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>> {
        Box::pin(async move {
            let launched = Manager::launch(&self, launch, source, None).await?;
            Ok(launched.space.id)
        })
    }
}

impl Manager {
    /// The load of each job starting or running, as admission counts it.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let loads = self.runner.job_loads(self).await?;
        Ok((loads.into_iter())
            .map(|load| Activity::Job {
                age: load.age,
                cost: load.cost,
                memory: load.memory,
            })
            .collect())
    }
}
